use super::*;
use crate::control::pagination::{self, Collection};

fn drain(revision: u64, draining: bool) -> Mutation {
    Mutation::SetNodeDrain {
        node: "local".into(),
        expected_epoch: 1,
        expected_revision: revision,
        draining,
    }
}
fn revoke(revision: u64) -> Mutation {
    Mutation::RevokeNode {
        node: "local".into(),
        expected_epoch: 1,
        expected_revision: revision,
    }
}
fn dispatch(task: &str) -> Mutation {
    Mutation::Dispatch {
        task: task.into(),
        expected_revision: 1,
        node: "local".into(),
        profile: "auto".into(),
    }
}

#[test]
fn node_drain_is_owner_revisioned_replayable_and_does_not_fence_existing_attempts() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, worker) = f.dispatch(&task);
    let next = f.task();
    for token in [&f.supervisor, &f.node, &worker] {
        assert!(f.store.apply(&request(token, drain(1, true))).is_err());
        assert!(f.store.node_status(token, "local").is_err());
    }
    assert!(
        f.store
            .apply_in_run(&f.run, &request(&f.owner, drain(1, true)))
            .is_err()
    );
    let op = request(&f.owner, drain(1, true));
    let result = f.store.apply(&op).unwrap();
    assert_eq!(result["revision"], 2);
    assert_eq!(result["epoch"], 1);
    assert_eq!(result["active_attempts"], 1);
    assert_eq!(result["active"], true);
    assert!(
        f.store
            .apply(&request(&f.supervisor, dispatch(&next)))
            .is_err()
    );
    assert!(f.store.apply(&request(&f.owner, drain(1, false))).is_err());
    assert!(f.store.apply(&request(&f.owner, revoke(2))).is_err());
    f.ready(&a, &worker);
    let p = pagination::page(&f.store, &f.supervisor, &f.run, Collection::Nodes, None, 50).unwrap();
    assert_eq!(p["items"][0]["value"]["draining"], true);
    call(&mut f.store, &f.owner, drain(2, false));
    assert_eq!(
        f.store.apply(&op).unwrap(),
        result,
        "replay is a historical receipt, not a new drain"
    );
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["draining"],
        false
    );
    let mut collision = request(&f.owner, drain(3, true));
    collision.operation_id = op.operation_id;
    assert!(f.store.apply(&collision).is_err());
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["revision"],
        3
    );
}

#[test]
fn node_revocation_requires_drain_and_confirmed_stop_and_preserves_signed_history() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, _) = f.dispatch(&task);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    let original = f.store.prepare_wire(&effect.id, &owner).unwrap();
    let wire = serde_json::to_string(&original).unwrap();
    assert!(f.store.apply(&request(&f.owner, revoke(1))).is_err());
    call(&mut f.store, &f.owner, drain(1, true));
    assert!(f.store.apply(&request(&f.owner, revoke(2))).is_err());
    let mut node = node_store(&f);
    node.receive(&original).unwrap();
    let stop = Request {
        token: f.node.clone(),
        operation_id: format!("stopped-{}-{}", a.id, a.epoch),
        command: Mutation::ObserveStopped {
            attempt: a.id.clone(),
            epoch: a.epoch,
        },
    };
    node.enqueue(&stop, &f.node_key).unwrap();
    let (event_id, event) = node.pending().unwrap().remove(0);
    node.published(&event_id).unwrap();
    assert!(
        f.store.apply(&request(&f.owner, revoke(2))).is_err(),
        "broker storage is not a controller stop"
    );
    f.store.receive(&event).unwrap();
    let receipt = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "receipt")
        .unwrap();
    node.receive(&f.store.prepare_wire(&receipt.id, &owner).unwrap())
        .unwrap();
    let req = request(&f.owner, revoke(2));
    let result = f.store.apply(&req).unwrap();
    assert_eq!(result["active"], false);
    assert_eq!(result["epoch"], 2);
    assert_eq!(result["revision"], 3);
    assert_eq!(result["active_attempts"], 0);
    assert_eq!(result["processes_signalled"], false);
    assert_eq!(result["broker_credentials_revoked"], false);
    assert!(result["retired_pending_effects"].as_u64().unwrap() >= 2);
    assert!(
        f.store.receive(&event).is_err(),
        "even an old replay must authenticate the current node"
    );
    assert!(f.store.apply(&stop).is_err());
    assert!(f.store.pending(&f.node, "local").is_err());
    assert!(f.store.prepare_wire(&effect.id, &owner).is_err());
    f.store.published(&effect.id).unwrap();
    let (saved, state): (String, String) = f
        .store
        .connection
        .query_row(
            "SELECT wire,state FROM outbox WHERE id=?1",
            [&effect.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(saved, wire);
    assert_eq!(state, "node_revoked");
    assert!(
        crate::control::delivery::controller_due(&f.store.connection)
            .unwrap()
            .is_empty()
    );
    assert!(
        f.store
            .node_grant("local", &f.node_key.did(), 1, now() + 1000)
            .is_err()
    );
    assert!(
        f.store
            .node_grant("local", &f.node_key.did(), 2, now() + 1000)
            .is_err()
    );
    assert!(f.store.apply(&request(&f.owner, drain(3, false))).is_err());
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(f.store.apply(&req).unwrap(), result);
    let p = pagination::page(&f.store, &f.owner, &f.run, Collection::Nodes, None, 50).unwrap();
    assert_eq!(
        p["items"][0]["value"]["active"], false,
        "retirement remains visible"
    );
    assert!(
        f.store
            .apply(&request(
                &f.owner,
                Mutation::EnrollNode {
                    id: "local".into(),
                    key: Identity::generate().unwrap().did(),
                    max_active: 1
                }
            ))
            .is_err()
    );
}

#[test]
fn node_revocation_rolls_back_authority_credentials_and_effects_with_failed_audit() {
    let mut f = Fixture::new();
    call(&mut f.store, &f.owner, drain(1, true));
    effect(
        &f.store.connection,
        "local",
        "cancel",
        &json!({"attempt":"retained"}),
        now() + 1000,
    )
    .unwrap();
    let before = f.store.node_status(&f.owner, "local").unwrap();
    f.store.connection.execute_batch("CREATE TRIGGER deny_revocation BEFORE INSERT ON journal WHEN NEW.kind='revoke_node' BEGIN SELECT RAISE(ABORT,'audit unavailable'); END;").unwrap();
    let req = request(&f.owner, revoke(2));
    assert!(f.store.apply(&req).is_err());
    assert_eq!(f.store.node_status(&f.owner, "local").unwrap(), before);
    assert_eq!(f.store.pending(&f.node, "local").unwrap().len(), 1);
    assert_eq!(
        f.store
            .connection
            .query_row(
                "SELECT count(*) FROM operations WHERE id=?1",
                [&req.operation_id],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER deny_revocation")
        .unwrap();
    assert_eq!(f.store.apply(&req).unwrap()["active"], false);
}

#[test]
fn node_grants_match_current_enrollment_and_schema_upgrade_preserves_old_signed_bytes() {
    let mut f = Fixture::new();
    let task = f.task();
    let _ = f.dispatch(&task);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    let original = f.store.prepare_wire(&effect.id, &owner).unwrap();
    assert!(
        f.store
            .node_grant("local", &f.node_key.did(), 1, now() + 1000)
            .is_ok()
    );
    assert!(
        f.store
            .node_grant("local", &owner.did(), 1, now() + 1000)
            .is_err()
    );
    assert!(
        f.store
            .node_grant("local", &f.node_key.did(), 2, now() + 1000)
            .is_err()
    );
    assert!(
        f.store
            .node_grant("unknown", &f.node_key.did(), 1, now() + 1000)
            .is_err()
    );
    f.store.connection.execute_batch("DROP TABLE event_recoveries; DROP TABLE coordinator_retirements; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision; PRAGMA user_version=9;").unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["revision"],
        1
    );
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["draining"],
        false
    );
    assert_eq!(
        f.store
            .prepare_wire(&effect.id, &owner)
            .unwrap()
            .content_hash,
        original.content_hash
    );
    f.store
        .connection
        .execute_batch("DROP TABLE coordinator_retirements; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; PRAGMA user_version=9;")
        .unwrap();
    assert!(
        Store::open(&f.store.dir).is_err(),
        "partial/inconsistent schema must not be adopted"
    );
    assert_eq!(
        f.store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        f.store
            .connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('nodes') WHERE name='draining'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        0,
        "failed migration rolls back its first ALTER"
    );
}

#[test]
fn node_drain_serializes_with_dispatch_without_releasing_or_duplicating_capacity() {
    for _ in 0..8 {
        let mut f = Fixture::new();
        let task = f.task();
        let gate = Arc::new(Barrier::new(2));
        let handles: Vec<_> = [
            request(&f.owner, drain(1, true)),
            request(&f.supervisor, dispatch(&task)),
        ]
        .into_iter()
        .map(|req| {
            let dir = f.store.dir.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&dir).unwrap();
                gate.wait();
                s.apply(&req).is_ok()
            })
        })
        .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(results[0]);
        let status = f.store.node_status(&f.owner, "local").unwrap();
        assert_eq!(status["active_attempts"], u64::from(results[1]));
        assert_eq!(status["draining"], true);
        assert_eq!(
            f.store.apply(&request(&f.owner, revoke(2))).is_ok(),
            !results[1]
        );
    }
}

#[tokio::test]
async fn node_revocation_real_broker_accepts_transport_but_controller_rejects_old_authority() {
    use crate::control::{
        broker_policy::Endpoint,
        transport::{Broker, controller_once, node_once},
    };
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
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
    let runtime = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    let mut node = node_store(&f);
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &runtime).await.unwrap();
    call(&mut f.store, &f.owner, drain(1, true));
    let req = request(
        &f.node,
        Mutation::ObserveStopped {
            attempt: attempt.id,
            epoch: 1,
        },
    );
    node.enqueue(&req, &f.node_key).unwrap();
    let (_, event) = node.pending().unwrap().remove(0);
    runtime.publish("controller", &event).await.unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &runtime).await.unwrap();
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["active_attempts"],
        0
    );
    assert_eq!(
        node.connection
            .query_row(
                "SELECT count(*) FROM events WHERE state='controller_applied'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
    call(&mut f.store, &f.owner, revoke(2));
    let mut delayed = event.record.clone();
    delayed.id = id();
    let delayed = f.node_key.sign(delayed).unwrap();
    runtime.publish("controller", &delayed).await.unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM reception_failures", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        f.store.node_status(&f.owner, "local").unwrap()["active"],
        false
    );
    let mut consumer = controller.consumer("controller").await.unwrap();
    assert_eq!(
        consumer.info().await.unwrap().num_ack_pending,
        0,
        "TERM follows durable rejection, not application acceptance"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn node_status_and_owner_drain_use_actual_api_and_mcp_authority() {
    use crate::control::api::{self, ApiRequest};
    let f = Fixture::new();
    let state = f.store.dir.clone();
    let serving = state.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        api::serve(serving, async {
            let _ = stopped.await;
        })
        .await
    });
    for _ in 0..100 {
        if state.join("control.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(state.join("control.sock").exists());
    let owner = f.owner.clone();
    let supervisor = f.supervisor.clone();
    tokio::task::spawn_blocking(move|| {
        assert!(api::call(&state,&ApiRequest::NodeStatus{token:supervisor.clone(),node:"local".into()}).is_err());
        let req=json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"supervisor_apply","arguments":{"operation_id":"drain-via-mcp","command":drain(1,true)}}}).to_string()+"\n";
        let mut out=Vec::new();api::mcp(&state,&owner,req.as_bytes(),&mut out).unwrap();
        let reply:Value=serde_json::from_slice(&out).unwrap();assert_eq!(reply["result"]["isError"],false);
        let req=json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"supervisor_node_status","arguments":{"node":"local"}}}).to_string()+"\n";
        let mut out=Vec::new();api::mcp(&state,&owner,req.as_bytes(),&mut out).unwrap();
        let reply:Value=serde_json::from_slice(&out).unwrap();
        let status:Value=serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(status["draining"],true);assert_eq!(status["revision"],2);
        assert!(!String::from_utf8(out).unwrap().contains(&owner));
        let mut out=Vec::new();api::mcp(&state,&supervisor,req.as_bytes(),&mut out).unwrap();
        let reply:Value=serde_json::from_slice(&out).unwrap();assert_eq!(reply["result"]["isError"],true);
    }).await.unwrap();
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
