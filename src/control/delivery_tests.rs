use super::*;
use crate::control::{
    broker_policy::Endpoint,
    delivery::{self, Queue},
    node::NodeStore,
    transport::{Broker, controller_once, node_once},
    worker::{self, WorkerRequest},
};
use rusqlite::params;

fn prepared() -> (Fixture, Attempt, String, NodeStore) {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
        .unwrap();
    (f, attempt, token, node)
}

fn proposal(f: &Fixture, a: &Attempt, token: &str, topic: &str) -> Request {
    request(
        token,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: topic.into(),
            text: "transport test evidence".into(),
            source_attempt: Some(a.id.clone()),
            evidence: vec!["isolated-test".into()],
        },
    )
}

fn insert_timed(
    node: &NodeStore,
    f: &Fixture,
    req: &Request,
    created: i64,
    expires: i64,
) -> (String, String, String) {
    let wire = f
        .node_key
        .sign(Record {
            record_type: "local.herdr.supervisor.event".into(),
            id: id(),
            owner: node.grant.record.owner.clone(),
            author: f.node_key.did(),
            recipient: node.grant.record.owner.clone(),
            epoch: 1,
            created_at: created,
            expires_at: expires,
            payload: serde_json::to_value(req).unwrap(),
        })
        .unwrap();
    let key = format!("{}:{}", digest(req.token.as_bytes()), req.operation_id);
    let text = serde_json::to_string(&wire).unwrap();
    node.connection
        .execute(
            "INSERT INTO events(id,request_hash,wire,state) VALUES (?1,?2,?3,'pending')",
            params![key, digest(&serde_json::to_vec(req).unwrap()), text],
        )
        .unwrap();
    (key, wire.record.id, text)
}

fn state(node: &NodeStore, key: &str) -> String {
    node.connection
        .query_row("SELECT state FROM events WHERE id=?1", [key], |r| r.get(0))
        .unwrap()
}

async fn brokers(f: &Fixture) -> (NatsServer, Broker, Broker) {
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = Broker::connect(&server.config("admin"), &owner, Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let node = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    (server, admin, node)
}

#[tokio::test]
async fn expired_malformed_and_future_outgoing_records_do_not_block_commands_or_receipts() {
    let (mut f, a, token, mut node) = prepared();
    let (_server, broker, node_broker) = brokers(&f).await;
    let expired_req = proposal(&f, &a, &token, "expired-note");
    let (expired, event, _) = insert_timed(&node, &f, &expired_req, now() - 60, now() - 1);
    node.connection.execute("INSERT INTO events(id,request_hash,wire,state) VALUES ('broken','broken','not-json','pending')",[]).unwrap();
    let (future, _, future_wire) = insert_timed(
        &node,
        &f,
        &proposal(&f, &a, &token, "future-note"),
        now() + 120,
        now() + 3600,
    );
    let valid = proposal(&f, &a, &token, "valid-note");
    let accepted = node.enqueue(&valid, &f.node_key).unwrap();
    // The execute order is already recorded locally, but its broker delivery is
    // still pending. A failed outgoing record used to prevent even reading it.
    controller_once(&mut f.store, &broker).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    let mut consumer = node_broker.consumer("local").await.unwrap();
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    assert_eq!(state(&node, &expired), "expired");
    assert_eq!(state(&node, "broken"), "invalid");
    let row: (String, String, u64, i64) = node
        .connection
        .query_row(
            "SELECT wire,state,tries,available_at FROM events WHERE id=?1",
            [&future],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(row.0, future_wire);
    assert_eq!(row.1, "pending");
    assert_eq!(row.2, 1);
    assert!(row.3 > now());
    // A legitimate receipt must still apply with the malformed row retained.
    controller_once(&mut f.store, &broker).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    let receipt = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Receipt {
            token: token.clone(),
            attempt: a.id.clone(),
            epoch: 1,
            event: accepted,
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "controller_applied");
    let receipt = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Receipt {
            token: token.clone(),
            attempt: a.id.clone(),
            epoch: 1,
            event: event.clone(),
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "expired");
    assert_eq!(receipt["result"], Value::Null);
    assert_eq!(node.enqueue(&expired_req, &f.node_key).unwrap(), event);
    assert_eq!(state(&node, &expired), "expired");
    let count: u64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM memory_proposals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        f.store.snapshot(&f.owner, &f.run).unwrap()["active_attempts"],
        1
    );
}

#[tokio::test]
async fn broker_acceptance_with_lost_sql_receipt_retries_identical_bytes_after_restart() {
    let (mut f, a, token, mut node) = prepared();
    let (_server, broker, node_broker) = brokers(&f).await;
    let req = proposal(&f, &a, &token, "retry-note");
    let event = node.enqueue(&req, &f.node_key).unwrap();
    let key = format!("{}:{}", digest(token.as_bytes()), req.operation_id);
    let wire: String = node
        .connection
        .query_row("SELECT wire FROM events WHERE id=?1", [&key], |r| r.get(0))
        .unwrap();
    node.connection.execute_batch("CREATE TRIGGER lose_publish_receipt BEFORE UPDATE ON events WHEN NEW.state='broker_stored' BEGIN SELECT RAISE(ABORT,'lost SQL receipt'); END;").unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(state(&node, &key), "pending");
    let count: u64 = node
        .connection
        .query_row("SELECT tries FROM events WHERE id=?1", [&key], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    assert!(delivery::node_due(&node.connection).unwrap().is_empty());
    let mut stream = broker.context.get_stream(&broker.stream).await.unwrap();
    assert_eq!(stream.info().await.unwrap().state.messages, 1);
    node.connection
        .execute_batch("DROP TRIGGER lose_publish_receipt;")
        .unwrap();
    drop(node);
    let mut node = node_store(&f);
    assert!(delivery::node_due(&node.connection).unwrap().is_empty());
    node.connection
        .execute("UPDATE events SET available_at=0 WHERE id=?1", [&key])
        .unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(
        stream.info().await.unwrap().state.messages,
        1,
        "broker dedup must retain the origin/record ID"
    );
    assert_eq!(
        node.connection
            .query_row("SELECT wire FROM events WHERE id=?1", [&key], |r| r
                .get::<_, String>(0))
            .unwrap(),
        wire
    );
    assert_eq!(node.enqueue(&req, &f.node_key).unwrap(), event);
    controller_once(&mut f.store, &broker).await.unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    assert_eq!(state(&node, &key), "controller_applied");
    let result = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Receipt {
            token,
            attempt: a.id,
            epoch: 1,
            event,
        },
    )
    .unwrap();
    assert_eq!(result["delivery"]["tries"], 2);
    assert_eq!(result["delivery"]["last_error"], "");
}

#[test]
fn retry_schedule_survives_restart_is_bounded_and_operator_projection_omits_credentials() {
    let (mut f, _, token, _node) = prepared();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    assert!(delivery::begin(&f.store.connection, Queue::Controller, &effect.id).unwrap());
    assert!(!delivery::begin(&f.store.connection, Queue::Controller, &effect.id).unwrap());
    assert!(f.store.pending(&f.node, "local").unwrap().is_empty());
    f.store = Store::open(&f.store.dir).unwrap();
    assert!(f.store.pending(&f.node, "local").unwrap().is_empty());
    for attempt in 2..=9 {
        f.store
            .connection
            .execute("UPDATE outbox SET available_at=0 WHERE id=?1", [&effect.id])
            .unwrap();
        let before = now();
        assert!(delivery::begin(&f.store.connection, Queue::Controller, &effect.id).unwrap());
        let (tries, next): (u64, i64) = f
            .store
            .connection
            .query_row(
                "SELECT tries,available_at FROM outbox WHERE id=?1",
                [&effect.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(tries, attempt);
        let delay = (1_i64 << attempt.min(6)).min(60);
        assert!((before + delay..=now() + delay).contains(&next));
    }
    let status = f.store.delivery_status(&f.node, "local").unwrap();
    assert_eq!(status["state_counts"]["pending"], 1);
    assert_eq!(status["recent"][0]["tries"], 9);
    assert!(!status.to_string().contains(&token));
    assert!(!status.to_string().contains("payload"));
    assert!(f.store.delivery_status(&f.node, "other").is_err());
    assert!(f.store.delivery_status(&f.supervisor, "local").is_err());
    f.store
        .connection
        .execute(
            "UPDATE outbox SET expires_at=?2 WHERE id=?1",
            params![effect.id, now() - 1],
        )
        .unwrap();
    let internal = crate::control::store::effect(
        &f.store.connection,
        "controller",
        "verify_candidate",
        &json!({}),
        now() - 1,
    )
    .unwrap();
    assert!(
        delivery::controller_due(&f.store.connection)
            .unwrap()
            .is_empty()
    );
    let status = f.store.delivery_status(&f.owner, "local").unwrap();
    assert_eq!(status["recent"][0]["state"], "expired");
    assert!(!delivery::begin(&f.store.connection, Queue::Controller, &effect.id).unwrap());
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT state FROM outbox WHERE id=?1", [internal], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        "pending"
    );
}

#[test]
fn node_v4_retry_migration_preserves_wire_jobs_memory_and_idempotent_operations() {
    let (f, a, token, mut node) = prepared();
    let req = proposal(&f, &a, &token, "migration-note");
    let event = node.enqueue(&req, &f.node_key).unwrap();
    let before = serde_json::to_value(node.pending().unwrap()).unwrap();
    node.connection.execute_batch("ALTER TABLE events DROP COLUMN tries; ALTER TABLE events DROP COLUMN available_at; ALTER TABLE events DROP COLUMN last_error; DROP TABLE reception_failures; PRAGMA user_version=4;").unwrap();
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(
        serde_json::to_value(node.pending().unwrap()).unwrap(),
        before
    );
    assert_eq!(node.enqueue(&req, &f.node_key).unwrap(), event);
    let receipt = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Receipt {
            token,
            attempt: a.id,
            epoch: 1,
            event,
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "pending");
    assert_eq!(receipt["delivery"]["tries"], 0);
    assert_eq!(receipt["delivery"]["next_attempt_at"], 0);
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM worker_memory", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        node.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

#[tokio::test]
async fn denied_outgoing_route_does_not_starve_incoming_events_or_other_nodes() {
    let (mut f, a, token, mut node) = prepared();
    let (server, admin, node_broker) = brokers(&f).await;
    let owner = f.store.owner().unwrap();
    let broker = Broker::connect(&server.config("controller"), &owner, Endpoint::Controller)
        .await
        .unwrap();
    let key = Identity::generate().unwrap();
    call(
        &mut f.store,
        &f.owner,
        Mutation::EnrollNode {
            id: "blocked".into(),
            key: key.did(),
            max_active: 1,
        },
    );
    // Simulate a newly enrolled route before the broker ACL has been updated.
    let bad = crate::control::store::effect(
        &f.store.connection,
        "blocked",
        "execute",
        &json!({}),
        now() + 3600,
    )
    .unwrap();
    f.store
        .connection
        .execute("UPDATE outbox SET available_at=0 WHERE id=?1", [&bad])
        .unwrap();
    let req = proposal(&f, &a, &token, "incoming-note");
    let event = node.enqueue(&req, &f.node_key).unwrap();
    node_once(&mut node, &node_broker).await.unwrap();
    controller_once(&mut f.store, &broker).await.unwrap();
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM memory_proposals", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    let status = f.store.delivery_status(&f.owner, "blocked").unwrap();
    assert_eq!(status["recent"][0]["state"], "pending");
    assert_eq!(status["recent"][0]["tries"], 1);
    assert!(status["recent"][0]["next_attempt_at"].as_i64().unwrap() > now());
    assert!(
        status["recent"][0]["last_error"]
            .as_str()
            .unwrap()
            .contains("unconfirmed")
    );
    node_once(&mut node, &node_broker).await.unwrap();
    let receipt = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Receipt {
            token,
            attempt: a.id,
            epoch: 1,
            event,
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "controller_applied");
    let mut stream = admin.context.get_stream(&admin.stream).await.unwrap();
    assert_eq!(
        stream.info().await.unwrap().state.messages,
        3,
        "only the node event, local execute and its receipt reach storage"
    );
}
