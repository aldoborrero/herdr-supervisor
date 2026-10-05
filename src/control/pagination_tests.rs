use super::*;
use crate::control::{
    api, coordinator,
    pagination::{self, Collection, PAGE_BYTES},
};

fn task(f: &mut Fixture, contract: Contract) -> Task {
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    serde_json::from_value(call(
        &mut f.store,
        &f.supervisor,
        Mutation::CreateTask {
            run: f.run.clone(),
            expected_revision: run.revision,
            contract,
            dependencies: vec![],
        },
    ))
    .unwrap()
}
fn scan(f: &Fixture, collection: Collection, limit: u32) -> Vec<Value> {
    let mut cursor = None;
    let mut values = Vec::new();
    loop {
        let page = pagination::page(
            &f.store,
            &f.owner,
            &f.run,
            collection,
            cursor.as_deref(),
            limit,
        )
        .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_BYTES);
        values.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x["value"].clone()),
        );
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(values.len() < 1000, "scan did not make progress");
    }
    values
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paginated_context_preserves_all_large_tasks_and_separates_growing_policy_maps() {
    let mut f = Fixture::new();
    let mut expected = BTreeMap::new();
    for n in 0..32 {
        let mut c = contract();
        c.title = format!("Task {n}");
        c.instructions = "é".repeat(16000);
        c.criteria = vec!["x".repeat(120000)];
        let t = task(&mut f, c);
        expected.insert(t.id, serde_json::to_value(t.contract).unwrap());
    }
    for n in 0..12 {
        let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        let mut profile = policy().profiles["auto"].clone();
        profile.argv = (0..100)
            .flat_map(|_| ["--model".into(), "x".repeat(256)])
            .collect();
        call(
            &mut f.store,
            &f.owner,
            Mutation::SetProfile {
                run: f.run.clone(),
                expected_revision: run.revision,
                name: format!("profile{n}"),
                profile: Some(profile),
            },
        );
    }
    assert!(
        serde_json::to_vec(&coordinator::context(&f.store, &f.owner, &f.run).unwrap())
            .unwrap()
            .len()
            > api::MAX_RESPONSE
    );
    let overview = pagination::overview(&f.store, &f.owner, &f.run).unwrap();
    assert!(serde_json::to_vec(&overview).unwrap().len() < PAGE_BYTES);
    assert!(overview["run"]["policy"].get("profiles").is_none());
    assert_eq!(overview["collections"]["tasks"]["count"], 32);
    assert_eq!(overview["collections"]["profiles"]["count"], 13);
    let tasks = scan(&f, Collection::Tasks, 100);
    assert_eq!(tasks.len(), expected.len());
    for t in tasks {
        assert_eq!(
            expected.remove(t["id"].as_str().unwrap()).unwrap(),
            t["contract"]
        );
    }
    assert!(expected.is_empty(), "no record omitted or duplicated");
    assert_eq!(scan(&f, Collection::Profiles, 3).len(), 13);
    assert_eq!(
        scan(&f, Collection::PolicyChecks, 1)[0]["value"],
        json!(["cargo", "test"])
    );
    assert!(!overview.to_string().contains(&f.owner));
    let state = f.store.dir.clone();
    let run = f.run.clone();
    let token = f.owner.clone();
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let path = state.clone();
    let server = tokio::spawn(async move {
        api::serve(path, async {
            let _ = stop.await;
        })
        .await
    });
    for _ in 0..200 {
        if state.join("control.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(state.join("control.sock").exists());
    let checked=tokio::task::spawn_blocking(move || {
        assert!(api::call(&state,&api::ApiRequest::CoordinatorContext {token:token.clone(),run:run.clone()}).unwrap_err().to_string().contains("too large"));
        let overview=api::call(&state,&api::ApiRequest::CoordinatorOverview {token:token.clone(),run:run.clone()}).unwrap();
        let credential = state.join("picker-token");
        secret_file(&credential, token.as_bytes()).unwrap();
        let binding = crate::managed::Binding { version: 1, controller: state.clone(), credential, run: run.clone(), launch_config: None };
        let picker = binding.overview().unwrap();
        assert!(picker.get("tasks").is_none());
        assert!(serde_json::to_vec(&picker).unwrap().len() <= PAGE_BYTES);
        assert!(crate::managed::summary(&picker).contains("32 tasks"));
        let mut cache = crate::managed_refresh::Cache::default();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            cache.poll();
            match cache.overview("demo", Ok(binding.clone())) {
                crate::managed_refresh::View::Current(value) => {
                    assert_eq!(crate::managed::summary(&value), crate::managed::summary(&picker));
                    assert!(value.get("tasks").is_none());
                    break;
                }
                crate::managed_refresh::View::Loading => {},
                other => panic!("unexpected picker state: {other:?}"),
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        for collection in Collection::ALL {
            let panel = binding.page(collection, None).unwrap();
            assert_eq!(panel["kind"],"project_page");
            assert_eq!(panel["_page"]["collection"],collection.name());
            if collection != Collection::Tasks { assert!(panel.get("tasks").is_none()); }
            else { assert!(!panel["tasks"].as_array().unwrap().is_empty()); assert!(panel["_page"]["next_cursor"].is_string()); }
            assert!(serde_json::to_vec(&panel).unwrap().len() <= 3 * PAGE_BYTES);
        }
        let cursor=overview["collections"]["tasks"]["cursor"].clone();
        let input=format!("{}\n",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"supervisor_page","arguments":{"run":run,"collection":"tasks","cursor":cursor,"limit":100}}}));
        let mut output=Vec::new();api::mcp(&state,&token,input.as_bytes(),&mut output).unwrap();
        assert!(output.len()<=api::MAX_RESPONSE);
        let reply:Value=serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["result"]["isError"],false);
        let page:Value=serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(!page["items"].as_array().unwrap().is_empty());
        assert!(page["next_cursor"].is_string());
        assert!(!String::from_utf8(output).unwrap().contains(&token));
    }).await;
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
    checked.unwrap();
}

#[test]
fn pagination_cursors_recheck_scope_authority_limits_and_policy_revision() {
    let mut f = Fixture::new();
    let a = f.task();
    f.task();
    let first =
        pagination::page(&f.store, &f.supervisor, &f.run, Collection::Tasks, None, 1).unwrap();
    let cursor = first["next_cursor"].as_str().unwrap();
    assert!(
        pagination::page(
            &f.store,
            &f.owner,
            &f.run,
            Collection::Tasks,
            Some(cursor),
            1
        )
        .is_err()
    );
    assert!(
        pagination::page(
            &f.store,
            &f.supervisor,
            &f.run,
            Collection::Attempts,
            Some(cursor),
            1
        )
        .is_err()
    );
    let other = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "other".into(),
            repo: "/other".into(),
            base_oid: BASE.into(),
            objective: "foreign".into(),
            policy: policy(),
        },
    );
    assert!(
        pagination::page(
            &f.store,
            &f.supervisor,
            other["id"].as_str().unwrap(),
            Collection::Tasks,
            Some(cursor),
            1
        )
        .is_err()
    );
    let (_, worker) = f.dispatch(&a);
    for token in [&worker, &f.node] {
        assert!(
            f.store.snapshot(token, &f.run).is_err(),
            "legacy snapshot cannot bypass page scope"
        );
        assert!(pagination::overview(&f.store, token, &f.run).is_err());
        assert!(pagination::page(&f.store, token, &f.run, Collection::Tasks, None, 1).is_err());
    }
    for limit in [0, 101, u32::MAX] {
        assert!(
            pagination::page(&f.store, &f.owner, &f.run, Collection::Tasks, None, limit).is_err()
        );
    }
    for bad in ["invalid".into(), "1".repeat(2049)] {
        assert!(
            pagination::page(&f.store, &f.owner, &f.run, Collection::Tasks, Some(&bad), 1).is_err()
        );
    }
    let profile_cursor=pagination::overview(&f.store,&f.owner,&f.run).unwrap()["collections"]["profiles"]["cursor"].as_str().unwrap().to_owned();
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let bound = call(
        &mut f.store,
        &f.owner,
        Mutation::BindSupervisor {
            run: f.run.clone(),
            expected_revision: run.revision,
        },
    );
    assert!(
        pagination::page(
            &f.store,
            &f.supervisor,
            &f.run,
            Collection::Tasks,
            Some(cursor),
            1
        )
        .is_err()
    );
    assert!(
        pagination::page(
            &f.store,
            bound["credential"].as_str().unwrap(),
            &f.run,
            Collection::Tasks,
            Some(cursor),
            1
        )
        .is_err()
    );
    assert!(
        pagination::page(
            &f.store,
            &f.owner,
            &f.run,
            Collection::Profiles,
            Some(&profile_cursor),
            1
        )
        .unwrap_err()
        .to_string()
        .contains("changed")
    );
    pagination::overview(&f.store, bound["credential"].as_str().unwrap(), &f.run).unwrap();
}

#[test]
fn pagination_reads_current_updates_but_new_rows_require_a_new_scan() {
    let mut f = Fixture::new();
    let a = f.task();
    let b = f.task();
    let c = f.task();
    let first = pagination::page(&f.store, &f.owner, &f.run, Collection::Tasks, None, 1).unwrap();
    assert_eq!(first["items"][0]["value"]["id"], a);
    let mut changed = contract();
    changed.title = "Changed between pages".into();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AmendTask {
            task: b.clone(),
            expected_revision: 1,
            contract: changed,
            dependencies: vec![],
        },
    );
    let d = f.task();
    let page = pagination::page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::Tasks,
        first["next_cursor"].as_str(),
        100,
    )
    .unwrap();
    assert_eq!(page["consistency"], "current_per_page");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["items"][0]["value"]["id"], b);
    assert_eq!(
        page["items"][0]["value"]["contract"]["title"],
        "Changed between pages"
    );
    assert_eq!(page["items"][1]["value"]["id"], c);
    assert!(page["next_cursor"].is_null());
    assert!(scan(&f, Collection::Tasks, 2).iter().any(|v| v["id"] == d));
}

#[test]
fn pagination_collections_preserve_provenance_withdrawal_and_hide_capabilities() {
    let mut f = Fixture::new();
    let t = f.task();
    let (attempt, token) = f.dispatch(&t);
    f.ready(&attempt, &token);
    call(
        &mut f.store,
        &token,
        Mutation::SubmitResult {
            attempt: attempt.id.clone(),
            epoch: 1,
            contract_hash: attempt.contract_hash.clone(),
            commit: Some(BASE.into()),
            artifacts: BTreeMap::new(),
            summary: "result evidence".into(),
        },
    );
    super::wire_limits_tests::approve(&mut f, "remember this");
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "new".into(),
            text: "unapproved".into(),
            source_attempt: Some(attempt.id),
            evidence: vec!["original evidence".into()],
        },
    );
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
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
    let secret: String = f
        .store
        .connection
        .query_row(
            "SELECT json_extract(doc,'$.credential') FROM coordinator_launches",
            [],
            |r| r.get(0),
        )
        .unwrap();
    for collection in Collection::ALL {
        for record in scan(&f, collection, 1) {
            assert!(!record.to_string().contains(&secret));
            assert!(!record.to_string().contains(&token));
        }
    }
    assert_eq!(
        scan(&f, Collection::PendingMemory, 1)[0]["id"],
        proposal["id"]
    );
    assert_eq!(
        scan(&f, Collection::PendingMemory, 1)[0]["evidence"],
        json!(["original evidence"])
    );
    assert_eq!(scan(&f, Collection::MemoryProposals, 1).len(), 2);
    call(
        &mut f.store,
        &f.owner,
        Mutation::WithdrawMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            expected_revision: 1,
        },
    );
    let memory = scan(&f, Collection::Memory, 1);
    assert_eq!(memory[0]["withdrawn"], true);
    assert_eq!(memory[0]["text"], "");
    assert_eq!(memory[0]["revision"], 2);
    assert_eq!(
        scan(&f, Collection::Results, 1)[0]["summary"],
        "result evidence"
    );
    assert_eq!(
        scan(&f, Collection::Attempts, 1)[0]["profile_snapshot"],
        serde_json::to_value(policy().profiles["auto"].clone()).unwrap()
    );
}

#[test]
fn pagination_oversized_legacy_record_fails_without_partial_json() {
    let mut f = Fixture::new();
    let t = f.task();
    f.store
        .connection
        .execute(
            "UPDATE tasks SET doc=?2 WHERE id=?1",
            rusqlite::params![t, json!({"oversized":"x".repeat(PAGE_BYTES)}).to_string()],
        )
        .unwrap();
    let error =
        pagination::page(&f.store, &f.owner, &f.run, Collection::Tasks, None, 100).unwrap_err();
    assert!(error.to_string().contains("no partial document"));
    assert_eq!(
        pagination::overview(&f.store, &f.owner, &f.run).unwrap()["collections"]["tasks"]["count"],
        1
    );
}

#[test]
fn pagination_legacy_memory_access_remains_inside_worker_or_reviewer_assignment() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    super::wire_limits_tests::approve(&mut f, "selected topic");
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "private_other_task".into(),
            text: "outside this assignment".into(),
            source_attempt: None,
            evidence: vec!["isolated evidence".into()],
        },
    );
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::ApproveMemory {
            proposal: proposal["id"].as_str().unwrap().into(),
            expected_revision: 0,
        },
    );
    assert_eq!(
        f.store
            .memory(&token, &f.run, &["parser".into()], 32000)
            .unwrap()["topics"]["parser"]["text"],
        "selected topic"
    );
    assert!(
        f.store
            .memory(&token, &f.run, &["private_other_task".into()], 32000)
            .is_err()
    );
    assert!(
        f.store
            .memory(&f.node, &f.run, &["parser".into()], 32000)
            .is_err()
    );
    assert!(
        f.store
            .memory(&token, "foreign", &["parser".into()], 32000)
            .is_err()
    );
    assert!(
        f.store
            .memory(&f.owner, &f.run, &["private_other_task".into()], 32000)
            .unwrap()["topics"]
            .get("private_other_task")
            .is_some()
    );
    // The same read rule applies to a reviewer's exact attempt; no role escalation
    // to a whole-run snapshot is implied by independent-review privileges.
    let mut review = a.clone();
    review.role = "reviewer".into();
    f.store
        .connection
        .execute(
            "UPDATE attempts SET role='reviewer',doc=?2 WHERE id=?1",
            rusqlite::params![review.id, serde_json::to_string(&review).unwrap()],
        )
        .unwrap();
    f.store
        .connection
        .execute(
            "UPDATE principals SET role='reviewer' WHERE attempt=?1",
            [&review.id],
        )
        .unwrap();
    assert!(f.store.snapshot(&token, &f.run).is_err());
    assert!(
        f.store
            .memory(&token, &f.run, &["private_other_task".into()], 32000)
            .is_err()
    );
    f.store
        .memory(&token, &f.run, &["parser".into()], 32000)
        .unwrap();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::StopAttempt {
            attempt: a.id,
            expected_epoch: 1,
        },
    );
    assert!(
        f.store
            .memory(&token, &f.run, &["parser".into()], 32000)
            .is_err()
    );
}
