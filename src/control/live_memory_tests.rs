use super::*;
use crate::control::{
    live_memory,
    node::NodeStore,
    worker::{self, WorkerRequest},
};

fn approve(f: &mut Fixture, topic: &str, text: &str, revision: u64) {
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: topic.into(),
            text: text.into(),
            source_attempt: None,
            evidence: vec![format!("git:{BASE}")],
        },
    );
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::ApproveMemory {
            proposal: proposal["id"].as_str().unwrap().into(),
            expected_revision: revision,
        },
    );
}
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
fn ack(
    f: &Fixture,
    node: &mut NodeStore,
    a: &Attempt,
    token: &str,
    version: u64,
) -> anyhow::Result<Value> {
    worker::handle(
        node,
        &f.node_key,
        WorkerRequest::AcknowledgeMemory {
            token: token.into(),
            attempt: a.id.clone(),
            epoch: a.epoch,
            version,
        },
    )
}

#[test]
fn live_memory_outbox_is_atomic_selected_and_retains_assignment_snapshot() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    approve(&mut f, "unrelated", "Do not send this to parser workers", 0);
    assert!(wires(&mut f, "memory_update").is_empty());
    f.store.connection.execute_batch("CREATE TRIGGER fail_memory BEFORE INSERT ON outbox WHEN NEW.kind='memory_update' BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "Use UTF-8 offsets".into(),
            source_attempt: Some(a.id.clone()),
            evidence: vec!["review:fixture".into()],
        },
    );
    let command = Mutation::ApproveMemory {
        proposal: text(&proposal, "id"),
        expected_revision: 0,
    };
    assert!(
        f.store
            .apply(&request(&f.supervisor, command.clone()))
            .is_err()
    );
    assert_eq!(
        live_memory::version(&f.store.connection, &f.run).unwrap(),
        1
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_memory")
        .unwrap();
    call(&mut f.store, &f.supervisor, command);
    let update = wires(&mut f, "memory_update").pop().unwrap();
    node.receive(&update).unwrap();
    let c = context(&f, &mut node, &a, &token);
    assert_eq!(c["memory"]["topics"], json!({}));
    assert_eq!(c["current_memory"]["version"], 2);
    assert_eq!(
        c["current_memory"]["snapshot"]["topics"]["parser"]["text"],
        "Use UTF-8 offsets"
    );
    assert!(
        c["current_memory"]["snapshot"]["topics"]
            .get("unrelated")
            .is_none()
    );
    assert_eq!(c["current_memory"]["acknowledged_version"], Value::Null);
    assert!(ack(&f, &mut node, &a, "wrong", 2).is_err());
    let mut wrong_epoch = a.clone();
    wrong_epoch.epoch = 2;
    assert!(ack(&f, &mut node, &wrong_epoch, &token, 2).is_err());
    assert!(ack(&f, &mut node, &a, &token, 1).is_err());
    assert_eq!(
        ack(&f, &mut node, &a, &token, 2).unwrap()["model_behavior_verified"],
        false
    );
    ack(&f, &mut node, &a, &token, 2).unwrap();
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["acknowledged_version"],
        2
    );
}

#[test]
fn reordered_memory_withdrawal_replay_and_cancellation_never_resurrect_topics() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    approve(&mut f, "parser", "Old parser rule", 0);
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::WithdrawMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            expected_revision: 1,
        },
    );
    let updates = wires(&mut f, "memory_update");
    assert_eq!(updates.len(), 2);
    let mut node = node_store(&f);
    assert!(
        node.receive(&updates[1]).is_err(),
        "update before execute must remain unacknowledged"
    );
    let rows: i64 = node
        .connection
        .query_row("SELECT count(*) FROM commands", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    node.receive(&updates[1]).unwrap();
    node.receive(&updates[0]).unwrap();
    node.receive(&updates[1]).unwrap();
    let current = context(&f, &mut node, &a, &token);
    assert_eq!(current["current_memory"]["version"], 2);
    assert_eq!(current["current_memory"]["snapshot"]["topics"], json!({}));
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut collision = updates[1].record.clone();
    collision.id = id();
    collision.payload["body"]["snapshot"] = updates[0].record.payload["body"]["snapshot"].clone();
    assert!(node.receive(&owner.sign(collision).unwrap()).is_err());
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::StopAttempt {
            attempt: a.id.clone(),
            expected_epoch: 1,
        },
    );
    node.receive(&wires(&mut f, "cancel")[0]).unwrap();
    let mut delayed = updates[0].record.clone();
    delayed.id = id();
    node.receive(&owner.sign(delayed).unwrap()).unwrap();
    assert!(ack(&f, &mut node, &a, &token, 2).is_err());
    assert_eq!(
        node.connection
            .query_row(
                "SELECT version FROM worker_memory WHERE attempt=?1",
                [&a.id],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        2
    );
}

#[test]
fn memory_rejects_foreign_topics_hashes_budget_run_and_future_epoch() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    approve(&mut f, "parser", "good", 0);
    let wire = wires(&mut f, "memory_update").pop().unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    for change in 0..6 {
        let mut record = wire.record.clone();
        record.id = id();
        let body = &mut record.payload["body"];
        match change {
            0 => body["run"] = json!(id()),
            1 => body["epoch"] = json!(2),
            2 => body["snapshot"]["topics"]["parser"]["hash"] = json!("wrong"),
            3 => {
                body["snapshot"]["topics"]["secret"] = body["snapshot"]["topics"]["parser"].clone();
            }
            4 => body["snapshot"]["omitted"] = json!(["secret"]),
            _ => {
                let text = "x".repeat(32001);
                body["snapshot"]["topics"]["parser"]["hash"] = json!(digest(text.as_bytes()));
                body["snapshot"]["topics"]["parser"]["text"] = json!(text);
                body["snapshot"]["bytes"] = json!(32001);
            }
        }
        assert!(
            node.receive(&owner.sign(record).unwrap()).is_err(),
            "case {change}"
        );
        assert_eq!(
            context(&f, &mut node, &a, &token)["current_memory"]["version"],
            0
        );
    }
    node.receive(&wire).unwrap();
}

#[test]
fn memory_versions_fence_updates_older_than_dispatch_and_migrate_v3_without_changing_jobs() {
    let mut f = Fixture::new();
    approve(&mut f, "parser", "old", 0);
    let stale = f
        .store
        .memory(&f.supervisor, &f.run, &["parser".into()], 32000)
        .unwrap();
    approve(&mut f, "parser", "latest", 1);
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&wires(&mut f, "execute")[0]).unwrap();
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["version"],
        2
    );
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut delayed = wires(&mut f, "execute")[0].record.clone();
    delayed.id = id();
    delayed.payload = json!({"kind":"memory_update","body":{"run":f.run,"attempt":a.id,"epoch":1,"version":1,"snapshot":stale}});
    node.receive(&owner.sign(delayed).unwrap()).unwrap();
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["version"],
        2
    );
    assert_eq!(
        node.connection
            .query_row("SELECT COUNT(*) FROM memory_notices", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    node.connection
        .execute(
            "UPDATE jobs SET doc=json_remove(doc,'$.memory_version') WHERE id=?1",
            [&a.id],
        )
        .unwrap();
    let old: String = node
        .connection
        .query_row("SELECT doc FROM jobs WHERE id=?1", [&a.id], |r| r.get(0))
        .unwrap();
    node.connection
        .execute_batch(
            "ALTER TABLE events DROP COLUMN tries; ALTER TABLE events DROP COLUMN available_at; ALTER TABLE events DROP COLUMN last_error; DROP TABLE memory_notices; DROP TABLE worker_memory; DROP TABLE artifact_staging; DROP TABLE reception_failures; DROP TABLE registration_recoveries; PRAGMA user_version=3;",
        )
        .unwrap();
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(
        node.connection
            .query_row("SELECT doc FROM jobs WHERE id=?1", [&a.id], |r| r
                .get::<_, String>(0))
            .unwrap(),
        old
    );
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["snapshot"]["topics"]["parser"]["text"],
        "latest"
    );
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["version"],
        0
    );
    ack(&f, &mut node, &a, &token, 0).unwrap();
    approve(&mut f, "parser", "newer", 2);
    node.receive(&wires(&mut f, "memory_update")[0]).unwrap();
    ack(&f, &mut node, &a, &token, 3).unwrap();
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(
        context(&f, &mut node, &a, &token)["current_memory"]["acknowledged_version"],
        3
    );
}

#[test]
fn native_memory_notice_replays_frozen_identity_after_bad_or_lost_receipt_and_retires_on_cancel() {
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
    approve(&mut f, "parser", "current", 0);
    node.receive(&wires(&mut f, "memory_update")[0]).unwrap();
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
        live_memory::deliver(&mut node, &config)
            .unwrap_err()
            .to_string()
            .contains("protocol 2")
    );
    assert!(
        live_memory::deliver(&mut node, &config).is_err(),
        "wrong recipient receipt cannot complete delivery"
    );
    let frozen: String = node
        .connection
        .query_row("SELECT prepared FROM memory_notices", [], |r| r.get(0))
        .unwrap();
    assert!(!frozen.contains(mailer_token));
    node.connection.execute_batch("CREATE TRIGGER lost_receipt BEFORE UPDATE OF state ON memory_notices WHEN NEW.state='submitted' BEGIN SELECT RAISE(ABORT,'lost receipt'); END;").unwrap();
    assert!(live_memory::deliver(&mut node, &config).is_err());
    node.connection
        .execute_batch("DROP TRIGGER lost_receipt")
        .unwrap();
    drop(node);
    let mut node = node_store(&f);
    std::fs::write(worker_dir.join("mailer.token"), "different-sender").unwrap();
    assert!(live_memory::deliver(&mut node, &config).is_err());
    std::fs::write(worker_dir.join("mailer.token"), mailer_token).unwrap();
    assert_eq!(
        live_memory::deliver(&mut node, &config).unwrap()["submitted"],
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
    approve(&mut f, "parser", "next", 1);
    node.receive(&wires(&mut f, "memory_update")[1]).unwrap();
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
        live_memory::deliver(&mut node, &config).unwrap()["submitted"],
        0
    );
    assert_eq!(
        node.connection
            .query_row(
                "SELECT state FROM memory_notices WHERE version=2",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "retired"
    );
}
