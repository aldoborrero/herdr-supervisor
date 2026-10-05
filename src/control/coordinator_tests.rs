use super::*;
use crate::control::coordinator::{self, Config, Endpoint};
use std::{
    io::{BufRead, Write},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

fn endpoint() -> Endpoint {
    Endpoint {
        agent: "coordinator".into(),
        mailer_session: id(),
        mailer_epoch: 1,
        runtime: "codex".into(),
        native_session: id(),
    }
}
fn bind(f: &mut Fixture, endpoint: Endpoint) -> Value {
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let response = call(
        &mut f.store,
        &f.owner,
        Mutation::BindCoordinator {
            run: f.run.clone(),
            expected_revision: run.revision,
            endpoint,
        },
    );
    f.supervisor = text(&response, "credential");
    response
}
fn propose(f: &mut Fixture) -> String {
    text(
        &call(
            &mut f.store,
            &f.supervisor,
            Mutation::ProposeMemory {
                run: f.run.clone(),
                topic: "parser".into(),
                text: "Use the corpus".into(),
                source_attempt: None,
                evidence: vec!["docs/corpus.md".into()],
            },
        ),
        "id",
    )
}
struct Mailer {
    config: Config,
    requests: Arc<Mutex<Vec<Value>>>,
    messages: Arc<Mutex<BTreeMap<String, Value>>>,
    stop: Arc<AtomicBool>,
    corrupt_receipt: Arc<AtomicBool>,
    changed_sender: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Mailer {
    fn new(f: &Fixture, lose_response: bool) -> Self {
        let state = f.dir.path().join("mailer");
        private_dir(&state).unwrap();
        let sender = state.join("sender.token");
        secret_file(&sender, b"test-mailer-sender-credential-000000000000000000").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(state.join("daemon.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let messages = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let corrupt_receipt = Arc::new(AtomicBool::new(false));
        let corrupt = corrupt_receipt.clone();
        let changed_sender = Arc::new(AtomicBool::new(false));
        let changed = changed_sender.clone();
        let (r, m, s) = (requests.clone(), messages.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            let mut lose = lose_response;
            while !s.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let author = if changed.load(Ordering::Relaxed) {
                    "another-sender"
                } else {
                    "notifier"
                };
                let data = match request["op"].as_str().unwrap() {
                    "capabilities" => json!({"idempotentSend":true,"recipientPrecondition":true}),
                    "doctor" => json!({"owner":"did:key:fixture", "agent":author}),
                    "send" => {
                        r.lock().unwrap().push(request.clone());
                        let operation =
                            format!("{author}/{}", request["operationId"].as_str().unwrap());
                        let mut messages = m.lock().unwrap();
                        let message=messages.entry(operation).or_insert_with(||json!({"id":id(),"owner":"did:key:fixture","author":author,"recipient":request["recipient"],"recipientSession":request["recipientSession"],"recipientEpoch":request["recipientEpoch"],"body":request["body"]})).clone();
                        assert_eq!(
                            message["body"], request["body"],
                            "retry changed its frozen content"
                        );
                        if lose {
                            lose = false;
                            continue;
                        }
                        if corrupt.load(Ordering::Relaxed) {
                            let mut bad = message;
                            bad["recipient"] = json!("another-agent");
                            bad
                        } else {
                            message
                        }
                    }
                    _ => panic!("unexpected mailer request"),
                };
                writeln!(stream, "{}", json!({"data":data})).unwrap();
            }
        });
        Self {
            config: Config {
                mailer_state: state,
                sender_credential: sender,
                supervisor_binary: "/original/supervisor".into(),
            },
            requests,
            messages,
            stop,
            corrupt_receipt,
            changed_sender,
            worker: Some(worker),
        }
    }
}
impl Drop for Mailer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn coordinator_context_is_scoped_and_exposes_memory_evidence_without_secrets() {
    let mut f = Fixture::new();
    let task = f.task();
    let (_, worker) = f.dispatch(&task);
    let old = f.supervisor.clone();
    let e = endpoint();
    let response = bind(&mut f, e);
    let proposal = propose(&mut f);
    coordinator::collect(&mut f.store).unwrap();
    assert!(coordinator::context(&f.store, &old, &f.run).is_err());
    assert!(coordinator::context(&f.store, &worker, &f.run).is_err());
    let context = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert_eq!(context["memory_proposals"][0]["id"], proposal);
    assert_eq!(
        context["memory_proposals"][0]["evidence"],
        json!(["docs/corpus.md"])
    );
    assert!(
        context["instructions"]
            .as_str()
            .unwrap()
            .contains("stop_attempt")
    );
    assert!(!context.to_string().contains(&f.supervisor));
    assert!(!context.to_string().contains(&f.owner));
    assert_eq!(
        super::super::runtime::credential(&coordinator::credential_path(
            &f.store.dir,
            &f.run,
            response["generation"].as_u64().unwrap()
        ))
        .unwrap(),
        f.supervisor
    );
    assert!(
        f.store
            .apply(&request(
                &worker,
                Mutation::AcknowledgeNotices {
                    run: f.run.clone(),
                    ids: vec![text(&context["notices"][0], "id")]
                }
            ))
            .is_err()
    );
}

#[test]
fn context_drains_unacknowledged_notices_across_multiple_pages() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    for _ in 0..105 {
        propose(&mut f);
    }
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 100);
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 5);
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
    let mut page_cursor = None;
    let mut paged_ids = std::collections::BTreeSet::new();
    loop {
        let page = crate::control::pagination::project_page(
            &f.store,
            &f.supervisor,
            &f.run,
            crate::control::pagination::Collection::PendingNotices,
            page_cursor.as_deref(),
        )
        .unwrap();
        assert_eq!(page["unacknowledged_notices"], 106);
        for notice in page["notices"].as_array().unwrap() {
            assert!(paged_ids.insert(text(notice, "id")));
            assert!(notice["acknowledged_by"].is_null());
        }
        page_cursor = page["_page"]["next_cursor"].as_str().map(str::to_owned);
        if page_cursor.is_none() {
            break;
        }
    }
    assert_eq!(paged_ids.len(), 106);
    let first = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert_eq!(first["unacknowledged_notices"], 106);
    let ids: Vec<String> = first["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| text(n, "id"))
        .collect();
    assert_eq!(ids.len(), 100);
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AcknowledgeNotices {
            run: f.run.clone(),
            ids: ids.clone(),
        },
    );
    let second = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert_eq!(second["unacknowledged_notices"], 6);
    for notice in &second["notices"].as_array().unwrap()[..6] {
        assert!(!ids.contains(&text(notice, "id")));
        assert!(notice["acknowledged_by"].is_null());
    }
}

#[test]
fn wrong_recipient_receipt_is_unresolved_and_idle_notifier_needs_no_mailer() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    let mailer = Mailer::new(&f, false);
    mailer.corrupt_receipt.store(true, Ordering::Relaxed);
    let result = coordinator::deliver(&mut f.store, &mailer.config).unwrap();
    assert_eq!(result["unresolved"], 1);
    assert_eq!(result["mailer_stored"], 0);
    let context = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert_eq!(context["notices"][0]["state"], "retry");
    assert!(context["notices"][0]["mailer_receipt"].is_null());
    mailer.corrupt_receipt.store(false, Ordering::Relaxed);
    f.store
        .connection
        .execute("UPDATE coordinator_notices SET available_at=0", [])
        .unwrap();
    assert_eq!(
        coordinator::deliver(&mut f.store, &mailer.config).unwrap()["mailer_stored"],
        1
    );
    let context = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert!(context["notices"][0]["mailer_receipt"].is_object());
    let requests = mailer.requests.lock().unwrap();
    assert_eq!(requests[0], requests[1]);
    drop(requests);
    let config = mailer.config.clone();
    drop(mailer);
    std::fs::remove_file(&config.sender_credential).unwrap();
    assert_eq!(
        coordinator::deliver(&mut f.store, &config).unwrap()["unresolved"],
        0
    );
}

#[test]
fn journal_cursor_and_notices_roll_back_together_and_restart_deduplicates() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    let before: i64 = f
        .store
        .connection
        .query_row("SELECT cursor FROM coordinator_routes", [], |r| r.get(0))
        .unwrap();
    propose(&mut f);
    f.store.connection.execute_batch("CREATE TRIGGER fail_notice BEFORE INSERT ON coordinator_notices WHEN NEW.kind='propose_memory' BEGIN SELECT RAISE(ABORT,'injected full disk'); END;").unwrap();
    assert!(coordinator::collect(&mut f.store).is_err());
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT cursor FROM coordinator_routes", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        before
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_notice")
        .unwrap();
    let path = f.store.dir.clone();
    f.store = Store::open(&path).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 1);
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
    let count: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM coordinator_notices", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let other = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "other".into(),
            repo: "/other".into(),
            base_oid: BASE.into(),
            objective: "separate".into(),
            policy: policy(),
        },
    );
    call(
        &mut f.store,
        &f.owner,
        Mutation::ProposeMemory {
            run: text(&other, "id"),
            topic: "parser".into(),
            text: "foreign".into(),
            source_attempt: None,
            evidence: vec!["other".into()],
        },
    );
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
}

#[test]
fn lost_mailer_response_or_sql_receipt_reuses_exact_target_and_frozen_body() {
    for lost_response in [true, false] {
        let mut f = Fixture::new();
        let target = endpoint();
        bind(&mut f, target.clone());
        let mut mailer = Mailer::new(&f, lost_response);
        if !lost_response {
            f.store.connection.execute_batch("CREATE TRIGGER lose_receipt BEFORE UPDATE ON coordinator_notices WHEN NEW.receipt IS NOT NULL BEGIN SELECT RAISE(ABORT,'lost SQL receipt'); END;").unwrap();
        }
        let first = coordinator::deliver(&mut f.store, &mailer.config);
        if lost_response {
            assert_eq!(first.unwrap()["unresolved"], 1);
        } else {
            assert!(first.is_err());
            f.store
                .connection
                .execute_batch("DROP TRIGGER lose_receipt")
                .unwrap();
        }
        assert_eq!(mailer.messages.lock().unwrap().len(), 1);
        let path = f.store.dir.clone();
        f.store = Store::open(&path).unwrap();
        f.store
            .connection
            .execute("UPDATE coordinator_notices SET available_at=0", [])
            .unwrap();
        mailer.config.supervisor_binary = "/updated/supervisor".into();
        let result = coordinator::deliver(&mut f.store, &mailer.config).unwrap();
        assert_eq!(result["mailer_stored"], 1);
        assert_eq!(result["model_processed"], false);
        let requests = mailer.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);
        assert_eq!(requests[0]["recipientSession"], target.mailer_session);
        assert_eq!(requests[0]["recipientEpoch"], 1);
        drop(requests);
        let context = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
        assert_eq!(context["notices"][0]["state"], "mailer_stored");
        call(
            &mut f.store,
            &f.supervisor,
            Mutation::AcknowledgeNotices {
                run: f.run.clone(),
                ids: vec![text(&context["notices"][0], "id")],
            },
        );
        coordinator::deliver(&mut f.store, &mailer.config).unwrap();
        assert_eq!(mailer.requests.lock().unwrap().len(), 2);
        assert_eq!(
            coordinator::context(&f.store, &f.supervisor, &f.run).unwrap()["notices"][0]["state"],
            "acknowledged"
        );
    }
}

#[test]
fn uncertain_send_cannot_be_retried_from_a_different_logical_author() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    let mailer = Mailer::new(&f, true);
    assert_eq!(
        coordinator::deliver(&mut f.store, &mailer.config).unwrap()["unresolved"],
        1
    );
    let path = f.store.dir.clone();
    f.store = Store::open(&path).unwrap();
    mailer.changed_sender.store(true, Ordering::Relaxed);
    f.store
        .connection
        .execute("UPDATE coordinator_notices SET available_at=0", [])
        .unwrap();
    assert_eq!(
        coordinator::deliver(&mut f.store, &mailer.config).unwrap()["unresolved"],
        1
    );
    assert_eq!(mailer.requests.lock().unwrap().len(), 1);
    assert_eq!(mailer.messages.lock().unwrap().len(), 1);
    mailer.changed_sender.store(false, Ordering::Relaxed);
    f.store
        .connection
        .execute("UPDATE coordinator_notices SET available_at=0", [])
        .unwrap();
    assert_eq!(
        coordinator::deliver(&mut f.store, &mailer.config).unwrap()["mailer_stored"],
        1
    );
    assert_eq!(mailer.messages.lock().unwrap().len(), 1);
    let requests = mailer.requests.lock().unwrap();
    assert_eq!(requests[0], requests[1]);
}

#[test]
fn rebinding_retires_old_capability_and_never_redirects_old_notice() {
    let mut f = Fixture::new();
    let target = endpoint();
    bind(&mut f, target.clone());
    let old_token = f.supervisor.clone();
    let mailer = Mailer::new(&f, true);
    coordinator::deliver(&mut f.store, &mailer.config).unwrap();
    let old_notice =
        coordinator::context(&f.store, &old_token, &f.run).unwrap()["notices"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
    let replacement = endpoint();
    bind(&mut f, replacement.clone());
    assert!(coordinator::context(&f.store, &old_token, &f.run).is_err());
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::AcknowledgeNotices {
                    run: f.run.clone(),
                    ids: vec![old_notice.clone()]
                }
            ))
            .is_err()
    );
    coordinator::deliver(&mut f.store, &mailer.config).unwrap();
    let requests = mailer.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["recipientSession"], target.mailer_session);
    assert_eq!(requests[1]["recipientSession"], replacement.mailer_session);
    assert_ne!(requests[0]["operationId"], requests[1]["operationId"]);
    assert_eq!(
        f.store
            .connection
            .query_row(
                "SELECT state FROM coordinator_notices WHERE id=?1",
                [old_notice],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "superseded"
    );
}

#[tokio::test]
async fn controller_service_delivers_coordinator_bootstrap_with_real_broker() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    let mailer = Mailer::new(&f, false);
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let config = server.config("admin");
    let broker = crate::control::transport::Broker::connect(
        &config,
        &owner,
        crate::control::broker_policy::Endpoint::Provisioner,
    )
    .await
    .unwrap();
    broker
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let path = f.dir.path().join("broker.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    crate::control::service::controller(
        f.store.dir.clone(),
        path,
        crate::control::git_pipeline::Config {
            repositories: BTreeMap::new(),
            check_timeout_secs: 1,
        },
        true,
        Some(mailer.config.clone()),
    )
    .await
    .unwrap();
    assert_eq!(mailer.messages.lock().unwrap().len(), 1);
    assert_eq!(
        coordinator::context(&f.store, &f.supervisor, &f.run).unwrap()["notices"][0]["state"],
        "mailer_stored"
    );
}

#[test]
fn amended_task_generates_one_coordinator_notice_and_replay_does_not_repeat_it() {
    let mut f = Fixture::new();
    let task = f.task();
    bind(&mut f, endpoint());
    let mut changed = contract();
    changed.title = "Replanned task".into();
    let req = request(
        &f.supervisor,
        Mutation::AmendTask {
            task: task.clone(),
            expected_revision: 1,
            contract: changed,
            dependencies: vec![],
        },
    );
    f.store.apply(&req).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 1);
    let notices = f.store.connection.query_row("SELECT COUNT(*) FROM coordinator_notices WHERE run=?1 AND aggregate_id=?2 AND kind='amend_task' AND state='pending'", rusqlite::params![f.run, task], |r| r.get::<_, u64>(0)).unwrap();
    assert_eq!(notices, 1);
    f.store.apply(&req).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
}

#[test]
fn operator_created_task_and_dependency_changes_wake_coordinator_once_each() {
    let mut f = Fixture::new();
    let predecessor = f.task();
    bind(&mut f, endpoint());
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let create = request(
        &f.owner,
        Mutation::CreateTask {
            run: f.run.clone(),
            expected_revision: run.revision,
            contract: contract(),
            dependencies: vec![],
        },
    );
    let task = f.store.apply(&create).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 1);
    f.store.apply(&create).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
    let dependency = request(
        &f.owner,
        Mutation::AddDependency {
            task: text(&task, "id"),
            expected_revision: 1,
            predecessor,
        },
    );
    f.store.apply(&dependency).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 1);
    f.store.apply(&dependency).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
    let context = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    for kind in ["create_task", "add_dependency"] {
        assert!(
            context["notices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["kind"] == kind && n["reference"] == task["id"])
        );
    }
}

#[test]
fn run_profiles_owner_update_is_visible_and_notifies_coordinator_once() {
    let mut f = Fixture::new();
    bind(&mut f, endpoint());
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let req = request(
        &f.owner,
        Mutation::SetProfile {
            run: f.run.clone(),
            expected_revision: run.revision,
            name: "new".into(),
            profile: Some(Profile {
                runtime: "pi".into(),
                argv: vec![],
                limit: 1,
            }),
        },
    );
    f.store.apply(&req).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 1);
    f.store.apply(&req).unwrap();
    assert_eq!(coordinator::collect(&mut f.store).unwrap(), 0);
    let owner = coordinator::context(&f.store, &f.owner, &f.run).unwrap();
    let supervisor = coordinator::context(&f.store, &f.supervisor, &f.run).unwrap();
    assert_eq!(owner["actor_role"], "owner");
    assert_eq!(supervisor["actor_role"], "supervisor");
    assert_eq!(
        supervisor["run"]["policy"]["profiles"]["new"]["runtime"],
        "pi"
    );
    assert!(
        supervisor["notices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "set_profile")
    );
}
