use super::*;
use crate::control::{event_recovery, node::NodeStore};

struct Setup {
    f: Fixture,
    node: NodeStore,
    attempt: Attempt,
    request: Request,
    original: SignedRecord,
    local: String,
}
impl Setup {
    fn new(expires: i64, stop: bool) -> Self {
        let mut f = Fixture::new();
        let task = f.task();
        let (attempt, token) = f.dispatch(&task);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let execute = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&execute.id, &owner).unwrap())
            .unwrap();
        let request = if stop {
            request(
                &f.node,
                Mutation::ObserveStopped {
                    attempt: attempt.id.clone(),
                    epoch: attempt.epoch,
                },
            )
        } else {
            request(
                &token,
                Mutation::ProposeMemory {
                    run: f.run.clone(),
                    topic: "parser".into(),
                    text: "retain this evidence".into(),
                    source_attempt: Some(attempt.id.clone()),
                    evidence: vec!["test fixture".into()],
                },
            )
        };
        node.enqueue(&request, &f.node_key).unwrap();
        let (local, old) = node.pending().unwrap().remove(0);
        let mut record = old.record;
        record.created_at = now() - 100;
        record.expires_at = expires;
        let original = f.node_key.sign(record).unwrap();
        node.connection
            .execute(
                "UPDATE events SET wire=?2 WHERE id=?1",
                rusqlite::params![local, serde_json::to_string(&original).unwrap()],
            )
            .unwrap();
        Self {
            f,
            node,
            attempt,
            request,
            original,
            local,
        }
    }
    fn recover(&mut self, op: &str) -> Value {
        self.node
            .recover_event(&self.original.record.id, op, &self.f.node_key)
            .unwrap()
    }
    fn envelope(&self, receipt: &Value) -> SignedRecord {
        let wire: String = self
            .node
            .connection
            .query_row(
                "SELECT wire FROM events WHERE json_extract(wire,'$.record.id')=?1",
                [receipt["recovery_event"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&wire).unwrap()
    }
    fn receipt(&mut self, delivery: &SignedRecord) {
        let effect = self
            .f
            .store
            .pending(&self.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "receipt" && e.payload["event_id"] == delivery.record.id)
            .unwrap();
        let owner = Identity::load(&self.f.store.dir.join("owner.key")).unwrap();
        self.node
            .receive(&self.f.store.prepare_wire(&effect.id, &owner).unwrap())
            .unwrap();
    }
    fn count(&self, table: &str) -> u64 {
        self.f
            .store
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn event_recovery_applies_expired_unseen_request_once_and_receipt_confirms_original_operation() {
    let mut s = Setup::new(now() - 1, false);
    assert!(s.f.store.receive(&s.original).is_err());
    let receipt = s.recover("recover");
    let replacement = s.envelope(&receipt);
    assert_eq!(replacement.record.record_type, event_recovery::TYPE);
    assert_eq!(
        serde_json::to_value(event_recovery::original(&replacement).unwrap().unwrap()).unwrap(),
        serde_json::to_value(&s.original).unwrap()
    );
    assert_eq!(s.recover("recover"), receipt);
    assert!(
        s.node
            .recover_event(&s.original.record.id, "duplicate-active", &s.f.node_key)
            .is_err()
    );
    let response = s.f.store.receive(&replacement).unwrap();
    assert_eq!(s.f.store.receive(&replacement).unwrap(), response);
    assert_eq!(s.count("memory_proposals"), 1);
    assert_eq!(s.count("event_recoveries"), 1);
    s.receipt(&replacement);
    let state = event_recovery::status(&s.node.dir, &s.original.record.id).unwrap();
    assert_eq!(state["controller_applied"], true);
    assert_eq!(state["recoveries"][0]["event"], receipt["recovery_event"]);
    assert!(!state.to_string().contains(&s.request.token));
    let receipt_wire:String=s.f.store.connection.query_row("SELECT wire FROM outbox WHERE kind='receipt' AND json_extract(payload,'$.event_id')=?1",[&replacement.record.id],|r|r.get(0)).unwrap();
    let mut conflicting: SignedRecord = serde_json::from_str(&receipt_wire).unwrap();
    conflicting.record.id = id();
    conflicting.record.payload["body"]["result"] = json!({"changed":true});
    let owner = Identity::load(&s.f.store.dir.join("owner.key")).unwrap();
    assert!(
        s.node
            .receive(&owner.sign(conflicting.record).unwrap())
            .is_err()
    );
    assert_eq!(
        event_recovery::status(&s.node.dir, &s.original.record.id).unwrap(),
        state
    );
    let saved: String = s
        .node
        .connection
        .query_row("SELECT wire FROM events WHERE id=?1", [&s.local], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap(),
        serde_json::to_value(&s.original).unwrap()
    );
    assert!(
        s.node
            .recover_event(&s.original.record.id, "after-acceptance", &s.f.node_key)
            .is_err()
    );
    drop(s.node);
    s.node = node_store(&s.f);
    assert_eq!(s.recover("recover"), receipt);
}

#[test]
fn event_recovery_returns_prior_acceptance_after_worker_retirement_but_never_new_old_authority() {
    let mut s = Setup::new(now() + 3, false);
    let response = s.f.store.receive(&s.original).unwrap();
    call(
        &mut s.f.store,
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    while now() < s.original.record.expires_at {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(s.f.store.apply(&s.request).is_err());
    let mut run: Run = document(&s.f.store.connection, "runs", &s.f.run).unwrap();
    run.deadline = now() - 1;
    save_run(&s.f.store.connection, &run).unwrap();
    let receipt = s.recover("lost-ack");
    let replacement = s.envelope(&receipt);
    assert_eq!(s.f.store.receive(&replacement).unwrap(), response);
    s.receipt(&replacement);
    assert_eq!(s.count("memory_proposals"), 1);
    // A different old event has a valid original signature but no accepted proof.
    let mut never = s.original.record.clone();
    never.id = id();
    let mut request: Request = serde_json::from_value(never.payload).unwrap();
    request.operation_id = id();
    never.payload = serde_json::to_value(request).unwrap();
    let mut retry = replacement.record.clone();
    retry.id = id();
    retry.payload = json!({"original":s.f.node_key.sign(never).unwrap()});
    let never = s.f.node_key.sign(retry).unwrap();
    let before = s.count("inbox");
    assert!(s.f.store.receive(&never).is_err());
    assert_eq!(s.count("inbox"), before);
    assert_eq!(s.count("memory_proposals"), 1);
}

#[test]
fn event_recovery_replays_a_prior_recovery_without_reapplying_and_rejects_origin_collisions() {
    let mut s = Setup::new(now() - 1, false);
    let receipt = s.recover("first");
    let replacement = s.envelope(&receipt);
    let response = s.f.store.receive(&replacement).unwrap();
    call(
        &mut s.f.store,
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    let mut again = replacement.record.clone();
    again.id = id();
    let again = s.f.node_key.sign(again).unwrap();
    assert_eq!(s.f.store.receive(&again).unwrap(), response);
    assert_eq!(s.count("memory_proposals"), 1);
    assert_eq!(s.count("event_recoveries"), 1);
    let mut conflict = s.original.record.clone();
    conflict.payload["operation_id"] = json!(id());
    let mut bad = replacement.record.clone();
    bad.id = id();
    bad.payload = json!({"original":s.f.node_key.sign(conflict).unwrap()});
    assert!(s.f.store.receive(&s.f.node_key.sign(bad).unwrap()).is_err());
    // Nested retries are never a way to grow authority or bypass original limits.
    let mut nested = replacement.record.clone();
    nested.id = id();
    nested.payload = json!({"original":replacement});
    assert!(
        s.f.store
            .receive(&s.f.node_key.sign(nested).unwrap())
            .is_err()
    );
}

#[test]
fn event_recovery_is_transactional_with_local_ledger_and_remote_receipt_failures() {
    let mut s = Setup::new(now() - 1, true);
    s.node.connection.execute_batch("CREATE TRIGGER fail_recovery BEFORE INSERT ON event_recoveries BEGIN SELECT RAISE(ABORT,'full'); END;").unwrap();
    assert!(
        s.node
            .recover_event(&s.original.record.id, "recover", &s.f.node_key)
            .is_err()
    );
    assert_eq!(
        s.node
            .connection
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    s.node
        .connection
        .execute_batch("DROP TRIGGER fail_recovery")
        .unwrap();
    let receipt = s.recover("recover");
    let replacement = s.envelope(&receipt);
    s.f.store.connection.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON outbox WHEN NEW.kind='receipt' BEGIN SELECT RAISE(ABORT,'full'); END;").unwrap();
    assert!(s.f.store.receive(&replacement).is_err());
    assert_eq!(s.count("inbox"), 0);
    assert_eq!(s.count("event_recoveries"), 0);
    assert_eq!(
        s.f.store.node_status(&s.f.owner, "local").unwrap()["active_attempts"],
        1
    );
    s.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_receipt")
        .unwrap();
    s.f.store.receive(&replacement).unwrap();
    assert_eq!(
        s.f.store.node_status(&s.f.owner, "local").unwrap()["active_attempts"],
        0
    );
    s.node.connection.execute_batch("CREATE TRIGGER fail_apply BEFORE UPDATE ON events WHEN NEW.state='controller_applied' BEGIN SELECT RAISE(ABORT,'full'); END;").unwrap();
    let effect =
        s.f.store
            .pending(&s.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "receipt")
            .unwrap();
    let owner = Identity::load(&s.f.store.dir.join("owner.key")).unwrap();
    let wire = s.f.store.prepare_wire(&effect.id, &owner).unwrap();
    assert!(s.node.receive(&wire).is_err());
    assert_eq!(
        event_recovery::status(&s.node.dir, &s.original.record.id).unwrap()["controller_applied"],
        false
    );
    s.node
        .connection
        .execute_batch("DROP TRIGGER fail_apply")
        .unwrap();
    s.node.receive(&wire).unwrap();
    assert_eq!(
        event_recovery::status(&s.node.dir, &s.original.record.id).unwrap()["controller_applied"],
        true
    );
}

#[test]
fn event_recovery_rejects_damaged_originals_foreign_signers_and_revoked_node_authority() {
    let mut s = Setup::new(now() - 1, false);
    let other = Identity::generate().unwrap();
    assert!(
        s.node
            .recover_event(&s.original.record.id, "foreign", &other)
            .is_err()
    );
    let saved = serde_json::to_string(&s.original).unwrap();
    s.node
        .connection
        .execute(
            "UPDATE events SET wire=json_set(wire,'$.signature','broken') WHERE id=?1",
            [&s.local],
        )
        .unwrap();
    assert!(
        s.node
            .recover_event(&s.original.record.id, "corrupt", &s.f.node_key)
            .is_err()
    );
    s.node
        .connection
        .execute(
            "UPDATE events SET wire=?2 WHERE id=?1",
            rusqlite::params![s.local, saved],
        )
        .unwrap();
    let receipt = s.recover("recover");
    let good = s.envelope(&receipt);
    call(
        &mut s.f.store,
        &s.f.owner,
        Mutation::EnrollNode {
            id: "other".into(),
            key: other.did(),
            max_active: 1,
        },
    );
    let mut foreign = good.record.clone();
    foreign.author = other.did();
    foreign.id = id();
    assert!(s.f.store.receive(&other.sign(foreign).unwrap()).is_err());
    let mut corrupt = good.record.clone();
    corrupt.id = id();
    corrupt.payload["original"]["signature"] = json!("broken");
    assert!(
        s.f.store
            .receive(&s.f.node_key.sign(corrupt).unwrap())
            .is_err()
    );
    let mut stale = good.record.clone();
    stale.id = id();
    stale.epoch += 1;
    assert!(
        s.f.store
            .receive(&s.f.node_key.sign(stale).unwrap())
            .is_err()
    );
    call(
        &mut s.f.store,
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    call(
        &mut s.f.store,
        &s.f.owner,
        Mutation::SetNodeDrain {
            node: "local".into(),
            expected_epoch: 1,
            expected_revision: 1,
            draining: true,
        },
    );
    call(
        &mut s.f.store,
        &s.f.owner,
        Mutation::RevokeNode {
            node: "local".into(),
            expected_epoch: 1,
            expected_revision: 2,
        },
    );
    assert!(
        s.f.store
            .receive(&good)
            .unwrap_err()
            .to_string()
            .contains("not enrolled")
    );
    assert_eq!(s.count("inbox"), 0);
    assert_eq!(s.count("event_recoveries"), 0);
}

#[test]
fn event_recovery_wraps_a_maximum_original_record_without_shrinking_its_budget() {
    let mut s = Setup::new(now() - 1, false);
    let mut original = s.original.record.clone();
    original.payload["command"]["params"]["evidence"] = json!([""]);
    let remaining = MAX_RECORD_BYTES - original.encoded().unwrap().len();
    original.payload["command"]["params"]["evidence"] = json!(["x".repeat(remaining)]);
    assert_eq!(original.encoded().unwrap().len(), MAX_RECORD_BYTES);
    s.original = s.f.node_key.sign(original.clone()).unwrap();
    s.request = serde_json::from_value(original.payload.clone()).unwrap();
    s.node
        .connection
        .execute(
            "UPDATE events SET wire=?2,request_hash=?3 WHERE id=?1",
            rusqlite::params![
                s.local,
                serde_json::to_string(&s.original).unwrap(),
                digest(&serde_json::to_vec(&s.request).unwrap())
            ],
        )
        .unwrap();
    let receipt = s.recover("max");
    let replacement = s.envelope(&receipt);
    assert!(replacement.record.encoded().unwrap().len() > MAX_RECORD_BYTES);
    assert!(serde_json::to_vec(&replacement).unwrap().len() < MAX_WIRE_BYTES);
    s.f.store.receive(&replacement).unwrap();
    s.receipt(&replacement);
    assert_eq!(s.count("memory_proposals"), 1);
    original.payload["command"]["params"]["evidence"] = json!(["x".repeat(remaining + 1)]);
    assert!(s.f.node_key.sign(original).is_err());
}

#[tokio::test]
async fn event_recovery_real_nats_renews_after_lost_expired_receipt_and_node_restart() {
    use crate::control::{
        broker_policy::Endpoint,
        transport::{Broker, controller_once, node_once},
    };
    let mut s = Setup::new(now() - 1, false);
    let owner = Identity::load(&s.f.store.dir.join("owner.key")).unwrap();
    let server = NatsServer::start(&owner.did());
    let admin = Broker::connect(&server.config("admin"), &owner.did(), Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let control = Broker::connect(
        &server.config("controller"),
        &owner.did(),
        Endpoint::Controller,
    )
    .await
    .unwrap();
    let runtime = Broker::connect(
        &server.config("node"),
        &owner.did(),
        Endpoint::Node("local"),
    )
    .await
    .unwrap();
    s.node.grant = owner
        .delegation("local", &s.f.node_key.did(), 1, now() + 3)
        .unwrap();
    let first = s.recover("first");
    let expires = first["expires_at"].as_i64().unwrap();
    node_once(&mut s.node, &runtime).await.unwrap();
    controller_once(&mut s.f.store, &control).await.unwrap();
    assert_eq!(s.count("memory_proposals"), 1);
    call(
        &mut s.f.store,
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    while now() < expires {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(s.node);
    s.node = node_store(&s.f);
    let next = s.recover("after-renewal");
    assert_ne!(first["recovery_event"], next["recovery_event"]);
    node_once(&mut s.node, &runtime).await.unwrap();
    controller_once(&mut s.f.store, &control).await.unwrap();
    node_once(&mut s.node, &runtime).await.unwrap();
    let status = event_recovery::status(&s.node.dir, &s.original.record.id).unwrap();
    assert_eq!(status["controller_applied"], true, "{status}");
    assert_eq!(status["recoveries"].as_array().unwrap().len(), 2);
    assert_eq!(s.count("memory_proposals"), 1);
    assert_eq!(s.count("event_recoveries"), 1);
    assert_eq!(
        s.node
            .connection
            .query_row(
                "SELECT count(*) FROM reception_failures WHERE state='expired'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn event_recovery_concurrent_envelopes_commit_one_logical_decision() {
    let mut s = Setup::new(now() - 1, false);
    let receipt = s.recover("concurrent");
    let original = s.envelope(&receipt);
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let mut record = original.record.clone();
            record.id = id();
            let signed = s.f.node_key.sign(record).unwrap();
            let dir = s.f.store.dir.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&dir).unwrap();
                barrier.wait();
                store.receive(&signed).unwrap()
            })
        })
        .collect();
    let responses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(responses.iter().all(|r| r == &responses[0]));
    assert_eq!(s.count("memory_proposals"), 1);
    assert_eq!(s.count("event_recoveries"), 1);
    assert_eq!(s.count("inbox"), 8);
}

#[test]
fn event_recovery_cannot_extend_a_run_deadline_but_can_confirm_a_late_stop() {
    for stop in [false, true] {
        let mut s = Setup::new(now() - 1, stop);
        let mut run: Run = document(&s.f.store.connection, "runs", &s.f.run).unwrap();
        run.deadline = now() - 1;
        save_run(&s.f.store.connection, &run).unwrap();
        let receipt = s.recover("late");
        let replacement = s.envelope(&receipt);
        let result = s.f.store.receive(&replacement);
        assert_eq!(result.is_ok(), stop);
        if !stop {
            assert!(result.unwrap_err().to_string().contains("deadline"));
            assert_eq!(s.count("inbox"), 0);
        }
        assert_eq!(s.count("memory_proposals"), 0);
        assert_eq!(
            s.f.store.node_status(&s.f.owner, "local").unwrap()["active_attempts"],
            if stop { 0 } else { 1 }
        );
    }
}
