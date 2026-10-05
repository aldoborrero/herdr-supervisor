use super::*;
use crate::control::node::NodeStore;

struct Setup {
    f: Fixture,
    node: NodeStore,
    attempt: Attempt,
    original: SignedRecord,
}

impl Setup {
    fn new(expires: i64) -> Self {
        let mut f = Fixture::new();
        let task = f.task();
        let (a, _) = f.dispatch(&task);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let execute = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&execute.id, &owner).unwrap())
            .unwrap();
        f.store.published(&execute.id).unwrap();
        let attempt: Attempt = serde_json::from_value(call(
            &mut f.store,
            &f.supervisor,
            Mutation::StopAttempt {
                attempt: a.id,
                expected_epoch: a.epoch,
            },
        ))
        .unwrap();
        let cancel = f
            .store
            .pending(&f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "cancel")
            .unwrap();
        let mut record = f.store.prepare_wire(&cancel.id, &owner).unwrap().record;
        record.created_at = now() - 100;
        record.expires_at = expires;
        let original = owner.sign(record).unwrap();
        f.store
            .connection
            .execute(
                "UPDATE outbox SET wire=?2,expires_at=?3 WHERE id=?1",
                rusqlite::params![
                    cancel.id,
                    serde_json::to_string(&original).unwrap(),
                    expires
                ],
            )
            .unwrap();
        Self {
            f,
            node,
            attempt,
            original,
        }
    }
    fn command(&self) -> Mutation {
        Mutation::RenewCancellation {
            attempt: self.attempt.id.clone(),
            expected_epoch: self.attempt.epoch,
            expired_command: self.original.record.id.clone(),
        }
    }
    fn renewed(&mut self) -> (Request, Value, SignedRecord) {
        let op = request(&self.f.supervisor, self.command());
        let receipt = self.f.store.apply(&op).unwrap();
        let owner = Identity::load(&self.f.store.dir.join("owner.key")).unwrap();
        let wire = self
            .f
            .store
            .prepare_wire(receipt["command"].as_str().unwrap(), &owner)
            .unwrap();
        (op, receipt, wire)
    }
    fn active(&self) -> u64 {
        self.f
            .store
            .connection
            .query_row("SELECT count(*) FROM attempts WHERE active=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn outbox(&self) -> u64 {
        self.f
            .store
            .connection
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn cancellation_recovery_delivers_unseen_stop_and_replays_without_releasing_capacity() {
    let mut s = Setup::new(now() - 1);
    let page = crate::control::pagination::page(
        &s.f.store,
        &s.f.supervisor,
        &s.f.run,
        crate::control::pagination::Collection::Attempts,
        None,
        50,
    )
    .unwrap();
    assert_eq!(
        page["items"][0]["value"]["cancellation_delivery"]["command"],
        s.original.record.id
    );
    assert_eq!(
        page["items"][0]["value"]["cancellation_delivery"]["expires_at"],
        s.original.record.expires_at
    );
    assert!(!page.to_string().contains(&s.f.node));
    assert!(s.node.receive(&s.original).is_err());
    let before = s.outbox();
    let (op, receipt, wire) = s.renewed();
    let page = crate::control::pagination::page(
        &s.f.store,
        &s.f.supervisor,
        &s.f.run,
        crate::control::pagination::Collection::Attempts,
        None,
        50,
    )
    .unwrap();
    assert_eq!(
        page["items"][0]["value"]["cancellation_delivery"]["command"],
        wire.record.id
    );
    assert_eq!(wire.record.payload, s.original.record.payload);
    assert_ne!(wire.record.id, s.original.record.id);
    assert_eq!(receipt["capacity_released"], false);
    assert_eq!(receipt["process_stop_confirmed"], false);
    assert_eq!(s.active(), 1);
    assert_eq!(s.f.store.apply(&op).unwrap(), receipt);
    assert_eq!(s.outbox(), before + 1);
    assert!(
        s.f.store
            .apply(&request(&s.f.supervisor, s.command()))
            .is_err()
    );
    s.node.receive(&wire).unwrap();
    s.node.receive(&wire).unwrap();
    let job: (u64, String) = s
        .node
        .connection
        .query_row(
            "SELECT epoch,state FROM jobs WHERE id=?1",
            [&s.attempt.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(job, (s.attempt.epoch, "cancelling".into()));
    assert_eq!(s.active(), 1);
    let saved: String =
        s.f.store
            .connection
            .query_row(
                "SELECT wire FROM outbox WHERE id=?1",
                [&s.original.record.id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(saved, serde_json::to_string(&s.original).unwrap());
    call(
        &mut s.f.store,
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    assert_eq!(s.active(), 0);
    s.f.store = Store::open(&s.f.store.dir).unwrap();
    assert_eq!(s.f.store.apply(&op).unwrap(), receipt);
    assert!(s.f.store.apply(&request(&s.f.owner, s.command())).is_err());
}

#[test]
fn cancellation_recovery_requires_scoped_current_intent_and_valid_original() {
    let mut current = Setup::new(now() + 3600);
    assert!(
        current
            .f
            .store
            .apply(&request(&current.f.owner, current.command()))
            .is_err()
    );
    let mut s = Setup::new(now() - 1);
    let before = s.outbox();
    assert!(s.f.store.apply(&request(&s.f.node, s.command())).is_err());
    assert!(
        s.f.store
            .apply_in_run(&id(), &request(&s.f.owner, s.command()))
            .is_err()
    );
    let mut stale = s.command();
    if let Mutation::RenewCancellation { expected_epoch, .. } = &mut stale {
        *expected_epoch -= 1;
    }
    assert!(s.f.store.apply(&request(&s.f.owner, stale)).is_err());
    let owner = Identity::load(&s.f.store.dir.join("owner.key")).unwrap();
    let mut damaged = s.original.clone();
    damaged.signature = "bad".into();
    s.f.store
        .connection
        .execute(
            "UPDATE outbox SET wire=?2 WHERE id=?1",
            rusqlite::params![
                s.original.record.id,
                serde_json::to_string(&damaged).unwrap()
            ],
        )
        .unwrap();
    assert!(s.f.store.apply(&request(&s.f.owner, s.command())).is_err());
    let mut wrong = s.original.record.clone();
    wrong.recipient = Identity::generate().unwrap().did();
    damaged = owner.sign(wrong).unwrap();
    s.f.store
        .connection
        .execute(
            "UPDATE outbox SET wire=?2 WHERE id=?1",
            rusqlite::params![
                s.original.record.id,
                serde_json::to_string(&damaged).unwrap()
            ],
        )
        .unwrap();
    assert!(s.f.store.apply(&request(&s.f.owner, s.command())).is_err());
    s.f.store
        .connection
        .execute(
            "UPDATE outbox SET wire=NULL WHERE id=?1",
            [&s.original.record.id],
        )
        .unwrap();
    // An outbox intent may expire before first signing; SQLite still owns it.
    s.f.store
        .apply_in_run(&s.f.run, &request(&s.f.owner, s.command()))
        .unwrap();
    assert_eq!(s.outbox(), before + 1);
    assert_eq!(s.active(), 1);
}

#[tokio::test]
async fn cancellation_recovery_private_api_and_mcp_share_scoped_operation_receipts() {
    use crate::control::api::{self, ApiRequest};
    let s = Setup::new(now() - 1);
    let state = s.f.store.dir.clone();
    let path = state.clone();
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        api::serve(path, async {
            let _ = stop.await;
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
    let command = s.command();
    let token = s.f.supervisor.clone();
    let node_token = s.f.node.clone();
    let result=tokio::task::spawn_blocking(move || {
        let operation=id();
        let input=format!("{}\n",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"supervisor_apply","arguments":{"operation_id":operation,"command":command}}}));
        let mut output=Vec::new();
        api::mcp(&state,&token,input.as_bytes(),&mut output).unwrap();
        assert!(!String::from_utf8_lossy(&output).contains(&token));
        let reply:Value=serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["result"]["isError"],false,"{reply}");
        let receipt:Value=serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        let applied=api::call(&state,&ApiRequest::Apply {token,operation_id:operation.clone(),command:Box::new(command.clone())}).unwrap();
        assert_eq!(receipt,applied);
        assert!(api::call(&state,&ApiRequest::Apply {token:node_token,operation_id:operation,command:Box::new(command)}).is_err());
        receipt
    }).await;
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert_eq!(result.unwrap()["capacity_released"], false);
    assert_eq!(s.active(), 1);
}

#[test]
fn cancellation_recovery_rolls_back_on_audit_failure_and_serializes_competing_renewals() {
    let mut s = Setup::new(now() - 1);
    let before = s.outbox();
    s.f.store.connection.execute_batch("CREATE TRIGGER reject_renewal BEFORE INSERT ON journal WHEN NEW.kind='renew_cancellation' BEGIN SELECT RAISE(ABORT,'test audit failure'); END;").unwrap();
    let op = request(&s.f.owner, s.command());
    assert!(s.f.store.apply(&op).is_err());
    assert_eq!(s.outbox(), before);
    s.f.store
        .connection
        .execute_batch("DROP TRIGGER reject_renewal;")
        .unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let dir = s.f.store.dir.clone();
            let req = request(&s.f.owner, s.command());
            std::thread::spawn(move || {
                let mut store = Store::open(&dir).unwrap();
                barrier.wait();
                store.apply(&req).is_ok()
            })
        })
        .collect();
    let count = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(count, 1);
    assert_eq!(s.outbox(), before + 1);
    assert_eq!(s.active(), 1);
}

#[test]
fn cancellation_recovery_node_requires_identical_persisted_epoch_and_never_resurrects_stopped_job()
{
    let mut s = Setup::new(now() + 3);
    s.node.receive(&s.original).unwrap();
    let owner = Identity::load(&s.f.store.dir.join("owner.key")).unwrap();
    let mut repeat = s.original.record.clone();
    repeat.id = id();
    repeat.expires_at = now() + 3600;
    let repeat = owner.sign(repeat).unwrap();
    s.node.receive(&repeat).unwrap();
    s.node
        .connection
        .execute(
            "UPDATE jobs SET state='stopped' WHERE id=?1",
            [&s.attempt.id],
        )
        .unwrap();
    let mut fresh = repeat.record.clone();
    fresh.id = id();
    s.node.receive(&owner.sign(fresh.clone()).unwrap()).unwrap();
    let state: String = s
        .node
        .connection
        .query_row("SELECT state FROM jobs WHERE id=?1", [&s.attempt.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(state, "stopped");
    fresh.id = id();
    fresh.payload["body"]["attempt"]["contract_hash"] = json!("changed");
    assert!(s.node.receive(&owner.sign(fresh).unwrap()).is_err());
    // A matching epoch without a recorded cancellation is not positive evidence.
    let mut unseen = Setup::new(now() - 1);
    unseen
        .node
        .connection
        .execute(
            "UPDATE jobs SET epoch=?2,state='cancelling' WHERE id=?1",
            rusqlite::params![unseen.attempt.id, unseen.attempt.epoch],
        )
        .unwrap();
    let (_, _, renewed) = unseen.renewed();
    assert!(unseen.node.receive(&renewed).is_err());
}

#[tokio::test]
async fn cancellation_recovery_real_broker_handles_expiry_restart_and_preexisting_cancellation() {
    use crate::control::{
        broker_policy::Endpoint,
        transport::{Broker, controller_once, node_once},
    };
    let mut s = Setup::new(now() + 3);
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
    controller_once(&mut s.f.store, &control).await.unwrap();
    node_once(&mut s.node, &runtime).await.unwrap();
    assert_eq!(
        s.node
            .connection
            .query_row("SELECT state FROM jobs WHERE id=?1", [&s.attempt.id], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "cancelling"
    );
    while now() < s.original.record.expires_at {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    drop(s.node);
    s.node = node_store(&s.f);
    let (op, receipt, _) = s.renewed();
    controller_once(&mut s.f.store, &control).await.unwrap();
    node_once(&mut s.node, &runtime).await.unwrap();
    let commands:u64=s.node.connection.query_row("SELECT count(*) FROM commands WHERE json_extract(record,'$.record.payload.kind')='cancel'",[],|r|r.get(0)).unwrap();
    assert_eq!(commands, 2);
    assert_eq!(s.active(), 1);
    assert_eq!(s.f.store.apply(&op).unwrap(), receipt);
    let event = request(
        &s.f.node,
        Mutation::ObserveStopped {
            attempt: s.attempt.id.clone(),
            epoch: s.attempt.epoch,
        },
    );
    s.node.enqueue(&event, &s.f.node_key).unwrap();
    node_once(&mut s.node, &runtime).await.unwrap();
    controller_once(&mut s.f.store, &control).await.unwrap();
    assert_eq!(s.active(), 0);
}
