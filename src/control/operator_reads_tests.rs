use super::*;
use crate::control::{
    api,
    pagination::{self, SessionObservation},
};

#[test]
fn opening_selects_only_authorized_profiles_and_reports_pending_launch_without_large_errors() {
    let mut f = Fixture::new();
    let read = |f: &Fixture, profile: Option<&str>| {
        pagination::open_context(&f.store, &f.owner, &f.run, profile).unwrap()
    };
    assert_eq!(read(&f, None)["selected_profile"], "auto");
    assert!(read(&f, Some("missing"))["selected_profile"].is_null());
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    run.policy
        .profiles
        .insert("second".into(), policy().profiles["auto"].clone());
    save_run(&f.store.connection, &run).unwrap();
    assert!(read(&f, None)["selected_profile"].is_null());
    assert_eq!(read(&f, Some("second"))["selected_profile"], "second");
    let config = super::coordinator_launch_tests::config(&f);
    let launch = call(
        &mut f.store,
        &f.owner,
        Mutation::StartCoordinator {
            run: f.run.clone(),
            expected_revision: run.revision,
            profile: "auto".into(),
            config,
        },
    );
    let initial = read(&f, None);
    assert_eq!(initial["pending_launch"]["id"], launch["id"]);
    for bytes in [16384, 16385] {
        let error = "e".repeat(bytes);
        f.store
            .connection
            .execute("UPDATE coordinator_launches SET last_error=?1", [&error])
            .unwrap();
        let view = read(&f, None);
        assert_eq!(view["pending_launch"]["last_error_bytes"], bytes);
        assert_eq!(view["pending_launch"]["last_error_omitted"], bytes > 16384);
        assert_eq!(
            view["pending_launch"]["last_error"],
            if bytes <= 16384 {
                json!(error)
            } else {
                Value::Null
            }
        );
        assert!(serde_json::to_vec(&view).unwrap().len() < 18000);
    }
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM coordinator_launches", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert!(pagination::open_context(&f.store, &f.node, &f.run, None).is_err());
    assert!(pagination::open_context(&f.store, &f.supervisor, "foreign", None).is_err());
}

#[test]
fn session_matching_checks_exact_active_resources_and_all_observations_before_answering() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, credential) = f.dispatch(&task);
    let resource = f.ready(&attempt, &credential);
    let observation = SessionObservation {
        session: resource.session,
        runtime: resource.runtime,
        workspace: resource.workspace,
        pane: resource.pane,
        terminal: resource.terminal,
    };
    let check = |observations: &[SessionObservation]| {
        pagination::matches_sessions(&f.store, &f.supervisor, &f.run, observations)
    };
    assert_eq!(
        check(std::slice::from_ref(&observation)).unwrap()["matched"],
        true
    );
    assert_eq!(check(&[]).unwrap()["matched"], false);
    for field in ["session", "runtime", "workspace", "pane", "terminal"] {
        let mut changed = serde_json::to_value(&observation).unwrap();
        changed[field] = json!(if field == "runtime" { "pi" } else { "wrong" });
        assert_eq!(
            check(&[serde_json::from_value(changed).unwrap()]).unwrap()["matched"],
            false
        );
    }
    let mut malformed = observation.clone();
    malformed.pane.clear();
    assert!(check(&[observation.clone(), malformed]).is_err());
    assert!(check(&vec![observation.clone(); 51]).is_err());
    assert_eq!(
        check(&vec![observation.clone(); 50]).unwrap()["matched"],
        true
    );
    assert!(
        pagination::matches_sessions(
            &f.store,
            &credential,
            &f.run,
            std::slice::from_ref(&observation)
        )
        .is_err()
    );
    assert!(
        pagination::matches_sessions(
            &f.store,
            &f.supervisor,
            "foreign",
            std::slice::from_ref(&observation)
        )
        .is_err()
    );
    f.store
        .connection
        .execute("UPDATE attempts SET active=0 WHERE id=?1", [&attempt.id])
        .unwrap();
    assert_eq!(check(&[observation]).unwrap()["matched"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_committed_reply_recovers_via_receipt_after_restart_without_reapplying() {
    let mut f = Fixture::new();
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let mut profile = policy().profiles["auto"].clone();
    profile.argv = (0..100)
        .flat_map(|_| ["--model".into(), "x".repeat(256)])
        .collect();
    for n in 0..165 {
        run.policy
            .profiles
            .insert(format!("profile{n}"), profile.clone());
    }
    run.policy.validate().unwrap();
    save_run(&f.store.connection, &run).unwrap();
    assert!(serde_json::to_vec(&run).unwrap().len() > api::MAX_RESPONSE);
    let command = Mutation::Pause {
        run: f.run.clone(),
        expected_revision: run.revision,
    };
    let operation_id = "recover-oversized-reply";
    let state = f.store.dir.clone();
    let path = state.clone();
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
    let token = f.owner.clone();
    let run_id = f.run.clone();
    let original_command = command.clone();
    let result = tokio::task::spawn_blocking(move || {
        let legacy = api::ApiRequest::ProjectApply {
            token: token.clone(),
            run: run_id.clone(),
            operation_id: operation_id.into(),
            command: Box::new(original_command.clone()),
        };
        assert!(
            api::call(&state, &legacy)
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
        let receipt = api::call(
            &state,
            &api::ApiRequest::ProjectApplyReceipt {
                token,
                run: run_id,
                operation_id: operation_id.into(),
                command: Box::new(original_command),
            },
        )
        .unwrap();
        assert_eq!(receipt["response_inline"], false);
        assert!(receipt["response"].is_null());
        assert_eq!(receipt["summary"]["state"], "paused");
        assert!(serde_json::to_vec(&receipt).unwrap().len() < 1024);
        receipt
    })
    .await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    let receipt = result.unwrap();
    let original = f
        .store
        .apply(&Request {
            token: f.owner.clone(),
            operation_id: operation_id.into(),
            command: command.clone(),
        })
        .unwrap();
    assert_eq!(
        receipt["response_sha256"],
        digest(&serde_json::to_vec(&original).unwrap())
    );
    call(
        &mut f.store,
        &f.owner,
        Mutation::Resume {
            run: f.run.clone(),
            expected_revision: run.revision + 1,
        },
    );
    let mut restarted = Store::open(&f.store.dir).unwrap();
    let replay = || api::ApiRequest::ProjectApplyReceipt {
        token: f.owner.clone(),
        run: f.run.clone(),
        operation_id: operation_id.into(),
        command: Box::new(command.clone()),
    };
    assert_eq!(api::handle(&mut restarted, replay()).unwrap(), receipt);
    assert_eq!(
        document::<Run>(&restarted.connection, "runs", &f.run)
            .unwrap()
            .state,
        "active"
    );
    assert!(
        api::handle(
            &mut restarted,
            api::ApiRequest::ProjectApplyReceipt {
                token: f.owner.clone(),
                run: "foreign".into(),
                operation_id: operation_id.into(),
                command: Box::new(command)
            }
        )
        .is_err()
    );
    restarted.connection.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON journal BEGIN SELECT RAISE(ABORT,'journal unavailable'); END;").unwrap();
    assert!(
        api::handle(
            &mut restarted,
            api::ApiRequest::ProjectApplyReceipt {
                token: f.owner.clone(),
                run: f.run.clone(),
                operation_id: "rolled-back".into(),
                command: Box::new(Mutation::Pause {
                    run: f.run.clone(),
                    expected_revision: run.revision + 2
                })
            }
        )
        .is_err()
    );
    assert_eq!(
        document::<Run>(&restarted.connection, "runs", &f.run)
            .unwrap()
            .state,
        "active"
    );
    assert_eq!(
        restarted
            .connection
            .query_row(
                "SELECT count(*) FROM operations WHERE id='rolled-back'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
}
