use super::*;
use crate::control::{
    node::NodeStore,
    pagination::{self, Collection},
    worker::{self, WorkerRequest},
};

fn wires(f: &mut Fixture, kind: &str) -> Vec<SignedRecord> {
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let ids: Vec<_> = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.id)
        .collect();
    ids.iter()
        .map(|id| f.store.prepare_wire(id, &owner).unwrap())
        .collect()
}
fn setup() -> (Fixture, Attempt, String, NodeStore) {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    f.ready(&a, &token);
    (f, a, token, node)
}
fn ask(a: &Attempt) -> Mutation {
    Mutation::AskQuestion {
        attempt: a.id.clone(),
        epoch: a.epoch,
        text: "Should offsets count UTF-8 bytes?".into(),
    }
}
fn answer(q: &Value) -> Mutation {
    Mutation::AnswerQuestion {
        question: text(q, "id"),
        expected_revision: 1,
        answer: "Yes, preserve the parser contract and count bytes.".into(),
    }
}
fn context(f: &Fixture, node: &mut NodeStore, a: &Attempt, token: &str) -> Value {
    worker::handle(
        node,
        &f.node_key,
        WorkerRequest::Context {
            token: token.into(),
            attempt: a.id.clone(),
            epoch: a.epoch,
        },
    )
    .unwrap()
}

#[test]
fn questions_scoped_roundtrip_reorders_replays_and_keeps_contract_immutable() {
    let (mut f, a, token, mut node) = setup();
    let before: Task = document(&f.store.connection, "tasks", &a.task).unwrap();
    let op = id();
    let queued = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Apply {
            token: token.clone(),
            operation_id: op.clone(),
            command: Box::new(ask(&a)),
        },
    )
    .unwrap();
    assert_eq!(queued["controller_applied"], false);
    let wire = node
        .pending()
        .unwrap()
        .into_iter()
        .find(|(_, w)| w.record.payload["operation_id"] == op)
        .unwrap()
        .1;
    let response = f.store.receive(&wire).unwrap();
    assert_eq!(f.store.receive(&wire).unwrap(), response);
    let q = response["question"].clone();
    let request = request(&f.supervisor, answer(&q));
    let resolved = f.store.apply(&request).unwrap();
    assert_eq!(f.store.apply(&request).unwrap(), resolved);
    let updates = wires(&mut f, "question_update");
    assert_eq!(updates.len(), 2);
    node.receive(&updates[1]).unwrap();
    node.receive(&updates[0]).unwrap();
    node.receive(&updates[1]).unwrap();
    let c = context(&f, &mut node, &a, &token);
    assert_eq!(c["questions"], json!([resolved["question"]]));
    assert_eq!(
        serde_json::to_value(document::<Task>(&f.store.connection, "tasks", &a.task).unwrap())
            .unwrap(),
        serde_json::to_value(before).unwrap()
    );
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM question_notices", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let page = pagination::page(
        &f.store,
        &f.supervisor,
        &f.run,
        Collection::Questions,
        None,
        50,
    )
    .unwrap();
    assert_eq!(page["items"][0]["value"], resolved["question"]);
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(
        context(&f, &mut node, &a, &token)["questions"],
        c["questions"]
    );
    let mut collision = updates[1].record.clone();
    collision.id = id();
    collision.payload["body"]["answer"] = json!("different resolution");
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    assert!(node.receive(&owner.sign(collision).unwrap()).is_err());
    assert_eq!(
        context(&f, &mut node, &a, &token)["questions"],
        c["questions"]
    );
}

#[test]
fn questions_authenticate_workers_supervisor_replacement_and_cancelled_attempts() {
    let (mut f, a, token, mut node) = setup();
    for credential in [&f.owner, &f.supervisor, &f.node] {
        assert!(f.store.apply(&request(credential, ask(&a))).is_err());
    }
    let q = call(&mut f.store, &token, ask(&a))["question"].clone();
    assert!(f.store.apply(&request(&token, answer(&q))).is_err());
    let second = f.task();
    let forged = Mutation::AskQuestion {
        attempt: second,
        epoch: a.epoch,
        text: "foreign".into(),
    };
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: token.clone(),
                operation_id: id(),
                command: Box::new(forged)
            }
        )
        .is_err()
    );
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let replacement = call(
        &mut f.store,
        &f.owner,
        Mutation::BindSupervisor {
            run: f.run.clone(),
            expected_revision: r.revision,
        },
    );
    assert!(f.store.apply(&request(&f.supervisor, answer(&q))).is_err());
    let next = text(&replacement, "credential");
    call(&mut f.store, &next, answer(&q));
    assert!(f.store.apply(&request(&next, answer(&q))).is_err());
    let open = call(&mut f.store, &token, ask(&a))["question"].clone();
    call(
        &mut f.store,
        &next,
        Mutation::StopAttempt {
            attempt: a.id.clone(),
            expected_epoch: a.epoch,
        },
    );
    assert!(f.store.apply(&request(&next, answer(&open))).is_err());
    node.receive(&wires(&mut f, "cancel")[0]).unwrap();
    for update in wires(&mut f, "question_update") {
        node.receive(&update).unwrap();
    }
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM question_notices", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Context {
                token,
                attempt: a.id,
                epoch: a.epoch
            }
        )
        .is_err()
    );
}

#[test]
fn questions_resolution_is_transactional_and_only_one_concurrent_answer_wins() {
    let (mut f, a, token, _) = setup();
    let q = call(&mut f.store, &token, ask(&a))["question"].clone();
    let before = storage_fault_tests::rows(&f.store.connection);
    f.store.connection.execute_batch("CREATE TRIGGER fail_answer BEFORE INSERT ON outbox WHEN NEW.kind='question_update' BEGIN SELECT RAISE(ABORT,'failure'); END;").unwrap();
    let req = request(&f.supervisor, answer(&q));
    assert!(f.store.apply(&req).is_err());
    assert_eq!(storage_fault_tests::rows(&f.store.connection), before);
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_answer;")
        .unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let path = f.store.dir.clone();
            let token = f.supervisor.clone();
            let barrier = barrier.clone();
            let mut cmd = answer(&q);
            if let Mutation::AnswerQuestion { answer, .. } = &mut cmd {
                *answer = format!("Resolution {i}");
            }
            std::thread::spawn(move || {
                let mut s = Store::open(&path).unwrap();
                barrier.wait();
                s.apply(&request(&token, cmd))
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|r| r.is_ok())
            .count(),
        1
    );
    assert_eq!(wires(&mut f, "question_update").len(), 2);
}

#[test]
fn questions_bound_text_count_and_preserve_foreign_run_privacy() {
    let (mut f, a, token, _) = setup();
    for body in ["".into(), " ".into(), "\0".into(), "é".repeat(2049)] {
        assert!(
            f.store
                .apply(&request(
                    &token,
                    Mutation::AskQuestion {
                        attempt: a.id.clone(),
                        epoch: a.epoch,
                        text: body
                    }
                ))
                .is_err()
        );
    }
    for _ in 0..32 {
        call(
            &mut f.store,
            &token,
            Mutation::AskQuestion {
                attempt: a.id.clone(),
                epoch: a.epoch,
                text: "é".repeat(2048),
            },
        );
    }
    assert!(f.store.apply(&request(&token, ask(&a))).is_err());
    assert!(pagination::page(&f.store, &token, &f.run, Collection::Questions, None, 50).is_err());
    let foreign = Fixture::new();
    assert!(
        pagination::page(
            &f.store,
            &foreign.supervisor,
            &f.run,
            Collection::Questions,
            None,
            50
        )
        .is_err()
    );
}

#[test]
fn questions_migration_preserves_old_jobs_and_failure_rolls_back_partial_schema() {
    let (mut f, _, _, node) = setup();
    f.store
        .connection
        .execute_batch("DROP TABLE task_questions; PRAGMA user_version=14;")
        .unwrap();
    let before = storage_fault_tests::rows(&f.store.connection);
    f.store = Store::open(&f.store.dir).unwrap();
    let mut after = storage_fault_tests::rows(&f.store.connection);
    assert!(after.remove("task_questions").unwrap().is_empty());
    assert_eq!(before, after);
    node.connection.execute_batch("DROP TABLE question_notices; DROP TABLE worker_questions; PRAGMA user_version=11; CREATE TABLE question_notices(sentinel TEXT) STRICT; INSERT INTO question_notices VALUES('preserve');").unwrap();
    let before = storage_fault_tests::rows(&node.connection);
    assert!(
        NodeStore::open(
            &node.dir,
            "local",
            &f.store.owner().unwrap(),
            node.grant.clone(),
            &f.node_key
        )
        .is_err()
    );
    assert_eq!(storage_fault_tests::rows(&node.connection), before);
    assert_eq!(
        node.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        11
    );
    node.connection
        .execute_batch("DROP TABLE question_notices;")
        .unwrap();
    let before = storage_fault_tests::rows(&node.connection);
    let node = node_store(&f);
    let mut after = storage_fault_tests::rows(&node.connection);
    assert!(after.remove("question_notices").unwrap().is_empty());
    assert!(after.remove("worker_questions").unwrap().is_empty());
    assert_eq!(before, after);
    assert_eq!(
        node.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        12
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn questions_real_mcp_ipc_routes_notices_and_exposes_managed_inbox() {
    use crate::control::{
        api,
        coordinator::{self, Endpoint},
    };
    let (mut f, a, token, mut node) = setup();
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let binding = call(
        &mut f.store,
        &f.owner,
        Mutation::BindCoordinator {
            run: f.run.clone(),
            expected_revision: r.revision,
            endpoint: Endpoint {
                agent: "coordinator".into(),
                mailer_session: id(),
                mailer_epoch: 1,
                runtime: "codex".into(),
                native_session: id(),
            },
        },
    );
    f.supervisor = text(&binding, "credential");
    f.node_key.save_new(&node.dir.join("node.key")).unwrap();
    let access = worker::Access {
        state: node.dir.clone(),
        node: "local".into(),
        owner: f.store.owner().unwrap(),
        grant: node.grant.clone(),
    };
    let controller = f.store.dir.clone();
    let worker_dir = node.dir.clone();
    let (stop_c, done_c) = tokio::sync::oneshot::channel();
    let (stop_n, done_n) = tokio::sync::oneshot::channel();
    let cserver = tokio::spawn(api::serve(controller.clone(), async {
        let _ = done_c.await;
    }));
    let nserver = tokio::spawn(worker::serve(access, async {
        let _ = done_n.await;
    }));
    for _ in 0..200 {
        if controller.join("control.sock").exists() && worker_dir.join("worker.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let result=tokio::task::spawn_blocking(move||{
        let op=id();let request=json!({"id":1,"method":"tools/call","params":{"name":"worker_apply","arguments":{"operation_id":op,"command":ask(&a)}}}).to_string()+"\n";
        let mut output=Vec::new();worker::mcp(&worker_dir,&token,&a.id,a.epoch,request.as_bytes(),&mut output).unwrap();
        let response:Value=serde_json::from_slice(&output).unwrap();assert_eq!(response["result"]["isError"],false);
        assert!(!String::from_utf8(output).unwrap().contains(&token));
        let wire=node.pending().unwrap().into_iter().find(|(_,w)|w.record.payload["operation_id"]==op).unwrap().1;
        let q=f.store.receive(&wire).unwrap()["question"].clone();
        coordinator::collect(&mut f.store).unwrap();
        assert_eq!(f.store.connection.query_row("SELECT count(*) FROM coordinator_notices WHERE kind='ask_question' AND aggregate_id=?1",[&a.id],|r|r.get::<_,i64>(0)).unwrap(),1);
        let view=pagination::project_page(&f.store,&f.supervisor,&f.run,Collection::Questions,None).unwrap();
        assert_eq!(view["questions"],json!([q]));
        let input=json!({"id":2,"method":"tools/call","params":{"name":"supervisor_apply","arguments":{"operation_id":id(),"command":answer(&q)}}}).to_string()+"\n";
        let mut output=Vec::new();api::mcp(&controller,&f.supervisor,input.as_bytes(),&mut output).unwrap();
        let response:Value=serde_json::from_slice(&output).unwrap();assert_eq!(response["result"]["isError"],false);
        assert!(!String::from_utf8(output).unwrap().contains(&f.supervisor));
        node.receive(&wires(&mut f,"question_update")[1]).unwrap();
        let input=json!({"id":3,"method":"tools/call","params":{"name":"worker_context","arguments":{}}}).to_string()+"\n";
        let mut output=Vec::new();worker::mcp(&worker_dir,&token,&a.id,a.epoch,input.as_bytes(),&mut output).unwrap();
        let response:Value=serde_json::from_slice(&output).unwrap();assert_eq!(response["result"]["isError"],false);
        let body:Value=serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["questions"][0]["revision"],2);
        // Keep the temporary root alive until both owned servers remove their
        // sockets; dropping it in this closure would race graceful shutdown.
        f
    }).await;
    stop_c.send(()).unwrap();
    stop_n.send(()).unwrap();
    cserver.await.unwrap().unwrap();
    nserver.await.unwrap().unwrap();
    result.unwrap();
}

#[test]
fn questions_native_notice_replays_frozen_identity_after_bad_or_lost_receipt_and_retires_on_cancel()
{
    use crate::control::{
        executor::{CheckoutPlan, PaneBinding},
        runtime::{LaunchPlan, RuntimeConfig},
    };
    use std::{
        io::{BufRead, Write},
        time::{Duration, Instant},
    };
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    let maildir = f.dir.path().join("mailer");
    private_dir(&maildir).unwrap();
    let worker_dir = node.dir.join("workers").join(&a.id);
    private_dir(&worker_dir).unwrap();
    let mailer_token = "private-mailer-token-for-memory-test";
    secret_file(&worker_dir.join("mailer.token"), mailer_token.as_bytes()).unwrap();
    let config = RuntimeConfig {
        supervisor_binary: "/bin/supervisor".into(),
        mailer_state: maildir.clone(),
        mailer_admin_credential: maildir.join("admin"),
        node_credential: node.dir.join("node.token"),
        codex_binary: "/bin/codex".into(),
        mailer_runtime_binary: "/bin/runtime".into(),
        claude_development_channel: false,
    };
    let receiver =
        json!({"agent":"worker-memory","id":id(),"epoch":1,"runtime":"codex","location":id()});
    let plan = LaunchPlan {
        checkout: CheckoutPlan {
            repo: f.dir.path().into(),
            common_dir: f.dir.path().join(".git"),
            worktree: f.dir.path().join("checkout"),
            branch: "fixture".into(),
            base_oid: BASE.into(),
            socket: f.dir.path().join("herdr.sock"),
            herdr_binary: "herdr".into(),
            creation_started: true,
            binding: Some(PaneBinding {
                workspace: "w".into(),
                pane: "p".into(),
                terminal: "t".into(),
            }),
        },
        config: config.clone(),
        expected_session: receiver["location"].as_str().map(str::to_owned),
        registration_started: true,
        registration: Some(receiver.clone()),
        launch_started: true,
        wrapper_started: true,
        wrapper_pid: None,
        bind_event: Some(id()),
        bootstrap: Some(json!({"id":id()})),
    };
    node.connection
        .execute(
            "UPDATE jobs SET state='bootstrapped',checkpoint=?2 WHERE id=?1",
            rusqlite::params![a.id, serde_json::to_string(&plan).unwrap()],
        )
        .unwrap();
    let q = call(&mut f.store, &token, ask(&a))["question"].clone();
    call(&mut f.store, &f.supervisor, answer(&q));
    node.receive(&wires(&mut f, "question_update")[1]).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(maildir.join("daemon.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let expected = receiver.clone();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let message = id();
        let mut capability_calls = 0;
        while requests.len() < 3 {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request = serde_json::from_str::<Value>(&line).unwrap();
            if request["op"] == "capabilities" {
                capability_calls += 1;
                writeln!(stream,"{}",json!({"data":{"idempotentSend":capability_calls>1,"recipientPrecondition":true}})).unwrap();
                continue;
            }
            assert_eq!(request["op"], "send");
            assert!(
                capability_calls > 1,
                "unsupported mailer must not receive a send"
            );
            requests.push(request);
            let session = if requests.len() == 1 {
                json!(id())
            } else {
                expected["id"].clone()
            };
            writeln!(
                stream,
                "{}",
                json!({"data":{"id":message,"recipientSession":session,"recipientEpoch":1}})
            )
            .unwrap();
        }
        requests
    });
    assert!(
        crate::control::question_delivery::deliver(&mut node, &config)
            .unwrap_err()
            .to_string()
            .contains("protocol 2")
    );
    assert!(
        crate::control::question_delivery::deliver(&mut node, &config).is_err(),
        "wrong recipient receipt cannot complete delivery"
    );
    let frozen: String = node
        .connection
        .query_row("SELECT prepared FROM question_notices", [], |r| r.get(0))
        .unwrap();
    assert!(!frozen.contains(mailer_token));
    node.connection.execute_batch("CREATE TRIGGER lost_receipt BEFORE UPDATE OF state ON question_notices WHEN NEW.state='submitted' BEGIN SELECT RAISE(ABORT,'lost receipt'); END;").unwrap();
    assert!(crate::control::question_delivery::deliver(&mut node, &config).is_err());
    node.connection
        .execute_batch("DROP TRIGGER lost_receipt")
        .unwrap();
    drop(node);
    let mut node = node_store(&f);
    std::fs::write(worker_dir.join("mailer.token"), "different-sender").unwrap();
    assert!(crate::control::question_delivery::deliver(&mut node, &config).is_err());
    std::fs::write(worker_dir.join("mailer.token"), mailer_token).unwrap();
    assert_eq!(
        crate::control::question_delivery::deliver(&mut node, &config).unwrap()["submitted"],
        1
    );
    let requests = server.join().unwrap();
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1], requests[2]);
    assert_eq!(requests[0]["recipientSession"], receiver["id"]);
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["acknowledged_version"],
        Value::Null
    );
    let next = call(&mut f.store, &token, ask(&a))["question"].clone();
    call(&mut f.store, &f.supervisor, answer(&next));
    node.receive(&wires(&mut f, "question_update")[3]).unwrap();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::StopAttempt {
            attempt: a.id.clone(),
            expected_epoch: 1,
        },
    );
    node.receive(&wires(&mut f, "cancel")[0]).unwrap();
    assert_eq!(
        crate::control::question_delivery::deliver(&mut node, &config).unwrap()["submitted"],
        0
    );
    assert_eq!(
        node.connection
            .query_row(
                "SELECT state FROM question_notices WHERE state='retired'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "retired"
    );
}
