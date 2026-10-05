use super::*;
use crate::control::{
    broker_policy::Endpoint,
    reception,
    transport::{Broker, controller_once, node_once},
};

async fn brokers(f: &Fixture) -> (NatsServer, Broker, Broker, Broker) {
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = Broker::connect(&server.config("admin"), &owner, Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let controller = Broker::connect(&server.config("controller"), &owner, Endpoint::Controller)
        .await
        .unwrap();
    let node = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    (server, admin, controller, node)
}
fn execute(f: &mut Fixture) -> SignedRecord {
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let item = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|p| p.kind == "execute")
        .unwrap();
    f.store.prepare_wire(&item.id, &owner).unwrap()
}
fn count(c: &rusqlite::Connection, table: &str) -> u64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
fn force_due(c: &rusqlite::Connection) {
    c.execute(
        "UPDATE reception_failures SET available_at=0 WHERE state='pending'",
        [],
    )
    .unwrap();
}
async fn raw(broker: &Broker, target: &str, bytes: Vec<u8>) {
    broker
        .context
        .publish(broker.subject(target).unwrap(), bytes.into())
        .await
        .unwrap()
        .await
        .unwrap();
}

#[tokio::test]
async fn poisoned_broker_windows_cannot_starve_valid_commands_and_events() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    let (_server, admin, controller, node_broker) = brokers(&f).await;
    for target in ["local", "controller"] {
        for index in 0..65 {
            raw(&admin, target, format!("not-json-{index}").into_bytes()).await;
        }
    }
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(count(&node.connection, "jobs"), 1);
    let req = request(
        &token,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "isolated evidence".into(),
            source_attempt: Some(a.id),
            evidence: vec!["test".into()],
        },
    );
    node.enqueue(&req, &f.node_key).unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(count(&f.store.connection, "memory_proposals"), 1);
    assert_eq!(count(&f.store.connection, "inbox"), 1);
    for c in [&node.connection, &f.store.connection] {
        let status = reception::status(c).unwrap();
        assert_eq!(status["state_counts"]["invalid"], 65);
        assert_eq!(status["recent"].as_array().unwrap().len(), 65);
        assert!(!status.to_string().contains("not-json"));
        assert!(!status.to_string().contains(&token));
    }
    for (broker, target) in [(&controller, "controller"), (&node_broker, "local")] {
        let mut consumer = broker.consumer(target).await.unwrap();
        assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    }
    // Duplicate rejected bytes cannot reset retries or consume another slot.
    raw(&admin, "local", b"not-json-0".to_vec()).await;
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(count(&node.connection, "reception_failures"), 65);
}

#[tokio::test]
async fn missing_assignment_retries_after_restart_and_lost_application_receipt() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, _) = f.dispatch(&task);
    let order = execute(&mut f);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut record = order.record.clone();
    record.id = id();
    record.payload = json!({"kind":"memory_update","body":{"run":f.run,"attempt":a.id,"epoch":1,"version":1,"snapshot":order.record.payload["body"]["memory"]}});
    let update = owner.sign(record).unwrap();
    let bytes = serde_json::to_vec(&update).unwrap();
    let (_server, admin, _controller, node_broker) = brokers(&f).await;
    let mut node = node_store(&f);
    admin.publish("local", &update).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(count(&node.connection, "commands"), 0);
    assert_eq!(
        reception::status(&node.connection).unwrap()["state_counts"]["pending"],
        1
    );
    let mut consumer = node_broker.consumer("local").await.unwrap();
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(reception::retry(&mut node).unwrap(), 0); // persistent backoff
    admin.publish("local", &order).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    force_due(&node.connection);
    node.connection.execute_batch("CREATE TRIGGER lose_reception_receipt BEFORE UPDATE ON reception_failures WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'lost receipt'); END;").unwrap();
    assert!(reception::retry(&mut node).is_err());
    assert_eq!(count(&node.connection, "commands"), 2); // application committed
    assert_eq!(
        reception::status(&node.connection).unwrap()["state_counts"]["pending"],
        1
    );
    node.connection
        .execute_batch("DROP TRIGGER lose_reception_receipt;")
        .unwrap();
    drop(node);
    let mut node = node_store(&f);
    force_due(&node.connection);
    assert_eq!(reception::retry(&mut node).unwrap(), 1);
    assert_eq!(count(&node.connection, "commands"), 2);
    assert_eq!(count(&node.connection, "memory_notices"), 1);
    let row: (Vec<u8>, String, u64) = node
        .connection
        .query_row("SELECT wire,state,tries FROM reception_failures", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (bytes, "applied".into(), 3));
    assert_eq!(
        reception::local_status(&node.dir, true).unwrap()["state_counts"]["applied"],
        1
    );
}

#[tokio::test]
async fn failed_local_hold_never_terminates_broker_delivery() {
    let mut f = Fixture::new();
    let (_server, admin, controller, node_broker) = brokers(&f).await;
    let mut node = node_store(&f);
    for c in [&node.connection, &f.store.connection] {
        c.execute_batch("CREATE TRIGGER deny_hold BEFORE INSERT ON reception_failures BEGIN SELECT RAISE(ABORT,'full'); END;").unwrap();
    }
    for target in ["local", "controller"] {
        raw(&admin, target, b"bad".to_vec()).await;
    }
    assert!(controller_once(&mut f.store, &controller).await.is_err());
    assert!(node_once(&mut node, &node_broker).await.is_err());
    for (broker, target) in [(&controller, "controller"), (&node_broker, "local")] {
        let mut consumer = broker.consumer(target).await.unwrap();
        assert_eq!(consumer.info().await.unwrap().num_ack_pending, 1);
    }
    assert_eq!(count(&node.connection, "commands"), 0);
    assert_eq!(count(&node.connection, "reception_failures"), 0);
    assert_eq!(count(&f.store.connection, "inbox"), 0);
    assert_eq!(count(&f.store.connection, "reception_failures"), 0);
}

#[tokio::test]
async fn deferred_controller_application_survives_restart_and_creates_one_receipt() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    node.receive(&execute(&mut f)).unwrap();
    let req = request(
        &token,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "retry evidence".into(),
            source_attempt: Some(a.id),
            evidence: vec!["test".into()],
        },
    );
    node.enqueue(&req, &f.node_key).unwrap();
    let (_server, _admin, controller, node_broker) = brokers(&f).await;
    f.store.connection.execute_batch("CREATE TRIGGER deny_proposal BEFORE INSERT ON memory_proposals BEGIN SELECT RAISE(ABORT,'temporary'); END;").unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    assert_eq!(count(&f.store.connection, "inbox"), 0);
    let mut consumer = controller.consumer("controller").await.unwrap();
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    let dir = f.store.dir.clone();
    f.store = Store::open(&dir).unwrap();
    f.store
        .connection
        .execute_batch("DROP TRIGGER deny_proposal;")
        .unwrap();
    force_due(&f.store.connection);
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(count(&f.store.connection, "inbox"), 1);
    assert_eq!(count(&f.store.connection, "memory_proposals"), 1);
    assert_eq!(
        reception::status(&f.store.connection).unwrap()["state_counts"]["applied"],
        1
    );
    assert_eq!(
        node.connection
            .query_row("SELECT state FROM events", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "controller_applied"
    );
    assert_eq!(reception::retry(&mut f.store).unwrap(), 0);
}

#[test]
fn reception_checks_integrity_time_scope_and_never_renews_or_leaks_records() {
    let mut f = Fixture::new();
    let task = f.task();
    f.dispatch(&task);
    let order = execute(&mut f);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut node = node_store(&f);
    let mut bad = order.clone();
    bad.record.payload = json!({"secret":"never-print-this"});
    reception::hold(&mut node.connection, &serde_json::to_vec(&bad).unwrap()).unwrap();
    let mut expired = order.record.clone();
    expired.created_at = now() - 120;
    expired.expires_at = now() - 1;
    reception::hold(
        &mut node.connection,
        &serde_json::to_vec(&owner.sign(expired).unwrap()).unwrap(),
    )
    .unwrap();
    let mut foreign = order.record.clone();
    foreign.recipient = Identity::generate().unwrap().did();
    let wire = serde_json::to_vec(&owner.sign(foreign).unwrap()).unwrap();
    reception::hold(&mut node.connection, &wire).unwrap();
    for _ in 0..8 {
        force_due(&node.connection);
        assert_eq!(reception::retry(&mut node).unwrap(), 0);
    }
    let (tries, available): (u64, i64) = node
        .connection
        .query_row(
            "SELECT tries,available_at FROM reception_failures WHERE state='pending'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(tries, 9);
    assert!((now() + 59..=now() + 60).contains(&available));
    assert_eq!(count(&node.connection, "commands"), 0);
    let status = reception::status(&node.connection).unwrap();
    assert_eq!(status["state_counts"]["invalid"], 1);
    assert_eq!(status["state_counts"]["expired"], 1);
    assert_eq!(status["state_counts"]["pending"], 1);
    assert!(!status.to_string().contains("never-print-this"));
    assert!(
        !status
            .to_string()
            .contains(order.record.payload["body"]["credential"].as_str().unwrap())
    );
    // Timestamp extremes must fail safely rather than overflow during classification.
    let mut extreme = order.record.clone();
    extreme.created_at = i64::MAX;
    extreme.expires_at = i64::MAX;
    reception::hold(
        &mut node.connection,
        &serde_json::to_vec(&owner.sign(extreme).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reception::status(&node.connection).unwrap()["state_counts"]["invalid"],
        2
    );
    node.connection
        .execute("UPDATE reception_failures SET updated=0", [])
        .unwrap();
    let pruned = reception::prune(&node.dir, true, now() - 1).unwrap();
    assert_eq!(pruned["removed"], 3);
    assert_eq!(count(&node.connection, "reception_failures"), 1);
    assert_eq!(
        node.connection
            .query_row("SELECT wire FROM reception_failures", [], |r| r
                .get::<_, Vec<u8>>(0))
            .unwrap(),
        wire
    );
    assert!(reception::prune(&node.dir, true, now() + 100).is_err());
}

#[test]
fn reception_limits_fail_closed_and_oversize_documents_retain_no_body() {
    let f = Fixture::new();
    let mut node = node_store(&f);
    let oversized = vec![b'x'; 300_001];
    reception::hold(&mut node.connection, &oversized).unwrap();
    let row: (Option<Vec<u8>>, u64, String) = node
        .connection
        .query_row("SELECT wire,bytes,state FROM reception_failures", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(row, (None, 300_001, "invalid".into()));
    node.connection
        .execute("UPDATE reception_failures SET wire=zeroblob(67108864)", [])
        .unwrap();
    assert!(reception::hold(&mut node.connection, b"one more body").is_err());
    assert_eq!(count(&node.connection, "reception_failures"), 1);
    node.connection
        .execute("UPDATE reception_failures SET wire=NULL", [])
        .unwrap();
    node.connection.execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<4095) INSERT INTO reception_failures SELECT 'filler-'||x,NULL,0,'invalid',1,0,'fixture',0,0 FROM n;").unwrap();
    assert!(reception::hold(&mut node.connection, b"over row cap").is_err());
    reception::hold(&mut node.connection, &oversized).unwrap(); // known replay remains safe at capacity
    assert_eq!(
        reception::status(&node.connection).unwrap()["recent"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    node.connection
        .execute("UPDATE reception_failures SET updated=0", [])
        .unwrap();
    assert_eq!(
        reception::prune(&node.dir, true, now() - 1).unwrap()["removed"],
        4096
    );
    reception::hold(&mut node.connection, b"space recovered").unwrap();
    assert_eq!(count(&node.connection, "reception_failures"), 1);
}

#[test]
fn schema_five_upgrade_preserves_existing_jobs_and_frozen_outbox() {
    let mut f = Fixture::new();
    let task = f.task();
    f.dispatch(&task);
    let wire = execute(&mut f);
    let mut node = node_store(&f);
    node.receive(&wire).unwrap();
    let before: String = f
        .store
        .connection
        .query_row("SELECT wire FROM outbox WHERE kind='execute'", [], |r| {
            r.get(0)
        })
        .unwrap();
    f.store
        .connection
        .execute_batch(
            "DROP TABLE integration_policies; DROP TABLE attempt_profiles; DROP TABLE artifact_chunks; DROP TABLE artifact_uploads; DROP TABLE artifact_retirements; DROP TABLE coordinator_retirements; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision;",
        )
        .unwrap();
    node.connection
        .execute_batch("DROP TABLE checkout_retirements; DROP TABLE artifact_staging;")
        .unwrap();
    for c in [&node.connection, &f.store.connection] {
        c.execute_batch("DROP TABLE reception_failures; DROP TABLE event_recoveries; DROP TABLE registration_recoveries; PRAGMA user_version=5;")
            .unwrap();
    }
    drop(node);
    let node = node_store(&f);
    f.store = Store::open(&f.store.dir).unwrap();
    for (c, version) in [(&node.connection, 10), (&f.store.connection, 14)] {
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
        assert_eq!(count(c, "reception_failures"), 0);
    }
    assert_eq!(count(&node.connection, "jobs"), 1);
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT wire FROM outbox WHERE kind='execute'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        before
    );
}

#[test]
fn frozen_reception_bytes_and_cancellation_fence_deferred_execution() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, _) = f.dispatch(&task);
    let order = execute(&mut f);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut node = node_store(&f);
    let wire = serde_json::to_vec(&order).unwrap();
    reception::hold(&mut node.connection, &wire).unwrap();
    let mut changed = order.record.clone();
    changed.id = id();
    let changed = serde_json::to_vec(&owner.sign(changed).unwrap()).unwrap();
    node.connection
        .execute("UPDATE reception_failures SET wire=?1", [changed])
        .unwrap();
    force_due(&node.connection);
    assert_eq!(reception::retry(&mut node).unwrap(), 0);
    assert_eq!(count(&node.connection, "jobs"), 0);
    assert_eq!(
        reception::status(&node.connection).unwrap()["state_counts"]["invalid"],
        1
    );
    // An independently received cancellation tombstone must fence the original
    // delayed execute, even though it is now being retried from SQLite.
    node.connection
        .execute("DELETE FROM reception_failures", [])
        .unwrap();
    reception::hold(&mut node.connection, &wire).unwrap();
    let mut cancel = order.record.clone();
    cancel.id = id();
    let mut cancelled = a.clone();
    cancelled.epoch = 2;
    cancelled.state = "cancelling".into();
    cancel.payload = json!({"kind":"cancel","body":{"attempt":cancelled}});
    node.receive(&owner.sign(cancel).unwrap()).unwrap();
    force_due(&node.connection);
    assert_eq!(reception::retry(&mut node).unwrap(), 1);
    let row: (u64, String) = node
        .connection
        .query_row("SELECT epoch,state FROM jobs", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(row, (2, "cancelling".into()));
    assert_eq!(count(&node.connection, "runtime_processes"), 0);
}
