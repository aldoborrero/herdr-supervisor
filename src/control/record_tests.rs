use super::*;
use crate::control::{
    api,
    pagination::{self, Collection},
    record,
};

fn large(f: &mut Fixture) -> (String, Value) {
    let task = f.task();
    let mut doc =
        serde_json::to_value(document::<Task>(&f.store.connection, "tasks", &task).unwrap())
            .unwrap();
    doc["legacy_evidence"] = json!("é🦀\u{0001}\\\"".repeat(80000));
    f.store
        .connection
        .execute(
            "UPDATE tasks SET doc=?2 WHERE id=?1",
            rusqlite::params![task, doc.to_string()],
        )
        .unwrap();
    let page = pagination::page(&f.store, &f.owner, &f.run, Collection::Tasks, None, 1).unwrap();
    (task, page["items"][0]["record"].clone())
}
fn cursor(reference: &Value) -> &str {
    reference["cursor"].as_str().unwrap()
}

fn assemble(f: &Fixture, reference: &Value) -> String {
    let mut cursor = Some(cursor(reference).to_owned());
    let mut text = String::new();
    while let Some(position) = cursor {
        let part = record::read(&f.store, &f.owner, &f.run, &position).unwrap();
        assert_eq!(part["offset"], text.len());
        text.push_str(part["text"].as_str().unwrap());
        cursor = part["next_cursor"].as_str().map(str::to_owned);
    }
    assert_eq!(reference["bytes"], text.len());
    assert_eq!(reference["sha256"], digest(text.as_bytes()));
    text
}

#[test]
fn record_chunks_preserve_named_policy_keys_and_exclude_private_launch_fields() {
    let mut f = Fixture::new();
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let argv = vec!["tool".to_string(), "e".repeat(800000)];
    run.policy.checks.insert("large_check".into(), argv.clone());
    run.policy.validate().unwrap();
    save_run(&f.store.connection, &run).unwrap();
    let page = pagination::page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::PolicyChecks,
        None,
        10,
    )
    .unwrap();
    let reference = &page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "large_check")
        .unwrap()["record"];
    assert_eq!(
        serde_json::from_str::<Value>(&assemble(&f, reference)).unwrap(),
        json!({"name":"large_check","value":argv})
    );
    let config = super::coordinator_launch_tests::config(&f);
    call(
        &mut f.store,
        &f.owner,
        Mutation::StartCoordinator {
            run: f.run.clone(),
            expected_revision: run.revision,
            profile: "auto".into(),
            config,
        },
    );
    f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET last_error=?1",
            ["diagnostic ".repeat(80000)],
        )
        .unwrap();
    let secret: String = f
        .store
        .connection
        .query_row(
            "SELECT json_extract(doc,'$.credential') FROM coordinator_launches",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!secret.is_empty());
    let page = pagination::page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::CoordinatorLaunches,
        None,
        1,
    )
    .unwrap();
    let text = assemble(&f, &page["items"][0]["record"]);
    assert!(!text.contains(&secret));
    assert!(!text.contains(&f.owner));
    let public: Value = serde_json::from_str(&text).unwrap();
    assert!(public.get("credential").is_none());
    assert_eq!(public["last_error"], "diagnostic ".repeat(80000));
}
fn forge(original: &str, field: &str, value: Value) -> String {
    let mut doc: Value =
        serde_json::from_slice(&bs58::decode(original).into_vec().unwrap()).unwrap();
    doc[field] = value;
    bs58::encode(serde_json::to_vec(&doc).unwrap()).into_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_record_round_trips_full_utf8_json_through_ipc_and_mcp_without_blocking_pages() {
    let mut f = Fixture::new();
    let (task, reference) = large(&mut f);
    f.task();
    let projection =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Tasks, None).unwrap();
    assert_eq!(projection["_page"]["shown"], 2);
    assert_eq!(projection["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(projection["record_references"].as_array().unwrap().len(), 1);
    let rows = crate::managed::page_view(
        "demo",
        crate::popup::Section::Tasks,
        Collection::Tasks,
        crate::managed_refresh::View::Current(projection),
    );
    assert!(rows.iter().any(|r| matches!(&r.kind,crate::popup::RowKind::Managed {doc,actions,..} if doc["record"]==reference && actions.is_empty())));
    let expected: String = f
        .store
        .connection
        .query_row(
            "SELECT json_set(doc,'$.dependencies',json('[]')) FROM tasks WHERE id=?1",
            [&task],
            |r| r.get(0),
        )
        .unwrap();
    let state = f.store.dir.clone();
    let path = state.clone();
    let token = f.owner.clone();
    let run = f.run.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        api::serve(path, async {
            let _ = stopped.await;
        })
        .await
    });
    for _ in 0..200 {
        if state.join("control.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let checked=tokio::task::spawn_blocking(move || {
        let mut next=Some(cursor(&reference).to_owned());let mut complete=String::new();let mut index=0;
        while let Some(cursor)=next {
            let part = if index%2==0 {
                api::call(&state,&api::ApiRequest::Record{token:token.clone(),run:run.clone(),cursor:cursor.clone()}).unwrap()
            } else {
                let request=format!("{}\n",json!({"jsonrpc":"2.0","id":index,"method":"tools/call","params":{"name":"supervisor_record","arguments":{"run":run,"cursor":cursor}}}));
                let mut output=Vec::new();api::mcp(&state,&token,request.as_bytes(),&mut output).unwrap();
                assert!(output.len()<api::MAX_RESPONSE);
                assert!(!String::from_utf8_lossy(&output).contains(&token));
                let reply:Value=serde_json::from_slice(&output).unwrap();
                assert_eq!(reply["result"]["isError"],false);
                serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
            };
            assert_eq!(part["offset"],complete.len());
            assert_eq!(part["sha256"],reference["sha256"]);
            let text=part["text"].as_str().unwrap();
            assert!(!text.is_empty() && text.len()<=record::CHUNK_BYTES);
            complete.push_str(text);
            assert_eq!(part["end"],complete.len());
            next=part["next_cursor"].as_str().map(str::to_owned);
            index+=1;assert!(index<200);
        }
        assert_eq!(complete,expected);
        assert_eq!(reference["bytes"],complete.len());
        assert_eq!(reference["sha256"],digest(complete.as_bytes()));
        assert!(serde_json::from_str::<Value>(&complete).unwrap()["legacy_evidence"].is_string());
    }).await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    checked.unwrap();
}

#[test]
fn record_reads_reauthorize_scope_reject_tampering_and_detect_same_length_changes() {
    let mut f = Fixture::new();
    let (task, reference) = large(&mut f);
    let original = cursor(&reference);
    let read = |cursor: &str| record::read(&f.store, &f.owner, &f.run, cursor);
    let first = read(original).unwrap();
    let next = first["next_cursor"].as_str().unwrap();
    assert_eq!(read(original).unwrap(), first);
    for (field, value) in [
        ("version", json!(2)),
        ("owner", json!("foreign")),
        ("run", json!("foreign")),
        ("principal", json!("foreign")),
        ("offset", reference["bytes"].clone()),
        ("bytes", json!(1)),
        ("sha256", json!("0".repeat(64))),
        ("key", json!("1 OR 1=1")),
    ] {
        assert!(read(&forge(original, field, value)).is_err(), "{field}");
    }
    assert!(read(&"x".repeat(2049)).is_err());
    let raw: String = f
        .store
        .connection
        .query_row(
            "SELECT json_set(doc,'$.dependencies',json('[]')) FROM tasks WHERE id=?1",
            [&task],
            |r| r.get(0),
        )
        .unwrap();
    let inside_utf8 = raw.find('é').unwrap() + 1;
    assert!(
        read(&forge(original, "offset", json!(inside_utf8)))
            .unwrap_err()
            .to_string()
            .contains("UTF-8 boundary")
    );
    assert!(record::read(&f.store, &f.supervisor, &f.run, original).is_err());
    assert!(record::read(&f.store, &f.owner, "foreign", original).is_err());
    let task2 = f.task();
    let (_, worker) = f.dispatch(&task2);
    assert!(record::read(&f.store, &worker, &f.run, original).is_err());
    let doc: String = f
        .store
        .connection
        .query_row("SELECT doc FROM tasks WHERE id=?1", [&task], |r| r.get(0))
        .unwrap();
    let changed = doc.replacen("legacy_evidence", "legacy_Evidence", 1);
    assert_eq!(doc.len(), changed.len());
    f.store
        .connection
        .execute(
            "UPDATE tasks SET doc=?2 WHERE id=?1",
            rusqlite::params![task, changed],
        )
        .unwrap();
    assert!(
        record::read(&f.store, &f.owner, &f.run, next)
            .unwrap_err()
            .to_string()
            .contains("record changed")
    );
    let fresh =
        pagination::page(&f.store, &f.supervisor, &f.run, Collection::Tasks, None, 1).unwrap();
    let fresh = cursor(&fresh["items"][0]["record"]);
    assert!(record::read(&f.store, &f.supervisor, &f.run, fresh).is_ok());
    f.store
        .connection
        .execute("UPDATE principals SET active=0 WHERE role='supervisor'", [])
        .unwrap();
    assert!(record::read(&f.store, &f.supervisor, &f.run, fresh).is_err());
}
