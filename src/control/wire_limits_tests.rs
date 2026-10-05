use super::*;
use crate::control::identity::{MAX_RECORD_BYTES, MAX_WIRE_BYTES};

pub(super) fn counts(f: &Fixture) -> Vec<u64> {
    [
        "attempts",
        "attempt_profiles",
        "principals",
        "outbox",
        "journal",
        "operations",
        "review_assignments",
    ]
    .iter()
    .map(|table| {
        f.store
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    })
    .collect()
}

pub(super) fn approve(f: &mut Fixture, text: &str) {
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: text.into(),
            source_attempt: None,
            evidence: vec!["isolated-fixture".into()],
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
}

#[test]
fn wire_limits_dispatch_rolls_back_capacity_credentials_and_all_effects() {
    let mut f = Fixture::new();
    approve(&mut f, &"\u{1}".repeat(32000));
    let mut contract = contract();
    contract.instructions = "x".repeat(65536);
    contract.criteria = vec!["y".repeat(8000)];
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let task = call(
        &mut f.store,
        &f.supervisor,
        Mutation::CreateTask {
            run: f.run.clone(),
            expected_revision: run.revision,
            contract,
            dependencies: vec![],
        },
    );
    let task: Task = serde_json::from_value(task).unwrap();
    let req = request(
        &f.supervisor,
        Mutation::Dispatch {
            task: task.id.clone(),
            expected_revision: task.revision,
            node: "local".into(),
            profile: "auto".into(),
        },
    );
    let before = counts(&f);
    assert!(
        f.store
            .apply(&req)
            .unwrap_err()
            .to_string()
            .contains("outgoing command is too large")
    );
    assert_eq!(
        counts(&f),
        before,
        "failed dispatch must not consume capacity, credentials, operation IDs or effects"
    );
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &task.id)
            .unwrap()
            .state,
        "planned"
    );
    // Withdraw the offending selection explicitly, then retry the SAME dispatch.
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::WithdrawMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            expected_revision: 1,
        },
    );
    let accepted = f.store.apply(&req).unwrap();
    assert_eq!(f.store.apply(&req).unwrap(), accepted);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effects = f.store.pending(&f.node, "local").unwrap();
    assert_eq!(effects.len(), 1);
    let signed = f.store.prepare_wire(&effects[0].id, &owner).unwrap();
    signed.verify(now()).unwrap();
    let mut node = node_store(&f);
    node.receive(&signed).unwrap();
    assert_eq!(
        signed.record.payload["body"]["contract"]["instructions"]
            .as_str()
            .unwrap()
            .len(),
        65536
    );
}

#[test]
fn wire_limits_signing_exact_encoded_boundary_and_signature_bound() {
    let key = Identity::generate().unwrap();
    for character in ['x', 'é', '\u{1}'] {
        let mut record = Record::command(
            &key.did(),
            key.did(),
            1,
            &id(),
            now() + 3600,
            "fixture",
            json!({"text":""}),
        );
        let empty = serde_json::to_vec(&record).unwrap().len();
        let encoded_char = serde_json::to_vec(&character.to_string()).unwrap().len() - 2;
        let budget = MAX_RECORD_BYTES - empty;
        record.payload["body"]["text"] = json!(format!(
            "{}{}",
            character.to_string().repeat(budget / encoded_char),
            "x".repeat(budget % encoded_char)
        ));
        let signed = key.sign(record.clone()).unwrap();
        assert_eq!(
            serde_json::to_vec(&signed.record).unwrap().len(),
            MAX_RECORD_BYTES
        );
        assert!(serde_json::to_vec(&signed).unwrap().len() < MAX_WIRE_BYTES);
        signed.verify(now()).unwrap();
        let mut oversized = record;
        let text = format!("{}x", oversized.payload["body"]["text"].as_str().unwrap());
        oversized.payload["body"]["text"] = json!(text);
        assert!(
            key.sign(oversized.clone())
                .unwrap_err()
                .to_string()
                .contains("encoded bytes")
        );
        let forged = SignedRecord {
            record: oversized,
            content_hash: signed.content_hash.clone(),
            signature: signed.signature.clone(),
        };
        assert!(
            forged
                .verify(now())
                .unwrap_err()
                .to_string()
                .contains("encoded bytes")
        );
        let mut bad_signature = signed;
        bad_signature.signature = "1".repeat(100000);
        assert!(
            bad_signature
                .verify(now())
                .unwrap_err()
                .to_string()
                .contains("signature length")
        );
    }
}

#[test]
fn wire_limits_node_rejects_oversized_event_before_durable_enqueue_and_allows_retry() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    let mut req = request(
        &token,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "evidence".into(),
            source_attempt: Some(attempt.id),
            evidence: vec![String::new()],
        },
    );
    let empty = serde_json::to_vec(&req.command).unwrap().len();
    if let Mutation::ProposeMemory { evidence, .. } = &mut req.command {
        evidence[0] = "x".repeat(MAX_RECORD_BYTES - 64 - empty);
    }
    assert!(serde_json::to_vec(&req.command).unwrap().len() < MAX_RECORD_BYTES);
    assert!(
        node.enqueue(&req, &f.node_key)
            .unwrap_err()
            .to_string()
            .contains("encoded bytes")
    );
    assert!(node.pending().unwrap().is_empty());
    if let Mutation::ProposeMemory { evidence, .. } = &mut req.command {
        evidence[0] = "bounded-evidence".into();
    }
    let event = node.enqueue(&req, &f.node_key).unwrap();
    assert_eq!(node.enqueue(&req, &f.node_key).unwrap(), event);
    assert_eq!(node.pending().unwrap().len(), 1);
}

#[tokio::test]
async fn wire_limits_large_execute_roundtrip_over_real_broker_preserves_full_contract() {
    use crate::control::{
        broker_policy::Endpoint,
        transport::{Broker, controller_once, node_once},
    };
    let mut f = Fixture::new();
    approve(&mut f, &"m".repeat(8000));
    let mut contract = contract();
    contract.instructions = "x".repeat(65536);
    contract.criteria = vec!["y".repeat(180000)];
    let expected = contract.clone();
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let task = call(
        &mut f.store,
        &f.supervisor,
        Mutation::CreateTask {
            run: f.run.clone(),
            expected_revision: run.revision,
            contract,
            dependencies: vec![],
        },
    );
    let (attempt, _) = f.dispatch(task["id"].as_str().unwrap());
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
    let node_broker = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    assert_eq!(controller_once(&mut f.store, &controller).await.unwrap(), 1);
    let wire: String = f
        .store
        .connection
        .query_row(
            "SELECT wire FROM outbox WHERE target='local' AND kind='execute'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let signed: SignedRecord = serde_json::from_str(&wire).unwrap();
    assert!(signed.record.encoded().unwrap().len() > 250000);
    let mut node = node_store(&f);
    assert_eq!(node_once(&mut node, &node_broker).await.unwrap(), 1);
    drop(node);
    let node = node_store(&f);
    let doc: String = node
        .connection
        .query_row("SELECT doc FROM jobs WHERE id=?1", [&attempt.id], |r| {
            r.get(0)
        })
        .unwrap();
    let order: crate::control::node::ExecuteOrder = serde_json::from_str(&doc).unwrap();
    assert_eq!(order.contract, expected);
    assert_eq!(order.memory["topics"]["parser"]["text"], "m".repeat(8000));
    let mut consumer = node_broker.consumer("local").await.unwrap();
    assert_eq!(
        consumer.info().await.unwrap().num_ack_pending,
        0,
        "ACK follows persistence of the complete order"
    );
}

#[test]
fn wire_limits_command_budget_includes_future_epoch_and_full_json_envelope() {
    let mut f = Fixture::new();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let mut template = Record::command(
        &owner.did(),
        f.node_key.did(),
        u64::MAX,
        &id(),
        i64::MAX,
        "fixture",
        json!({"text":""}),
    );
    template.created_at = i64::MIN;
    let budget = MAX_RECORD_BYTES - template.encoded().unwrap().len();
    let payload = json!({"text":"x".repeat(budget)});
    let accepted = effect(
        &f.store.connection,
        "local",
        "fixture",
        &payload,
        now() + 3600,
    )
    .unwrap();
    let before = counts(&f);
    assert!(
        effect(
            &f.store.connection,
            "local",
            "fixture",
            &json!({"text":"x".repeat(budget+1)}),
            now() + 3600
        )
        .is_err()
    );
    assert_eq!(counts(&f), before);
    f.store
        .connection
        .execute("UPDATE nodes SET epoch=?1 WHERE id='local'", [i64::MAX])
        .unwrap();
    let wire = f.store.prepare_wire(&accepted, &owner).unwrap();
    wire.verify(now()).unwrap();
    assert_eq!(wire.record.payload["body"], payload);
    let encoded = serde_json::to_vec(&wire).unwrap();
    let mut padded = encoded;
    padded.resize(MAX_WIRE_BYTES, b' ');
    crate::control::transport::decode(&padded)
        .unwrap()
        .verify(now())
        .unwrap();
    padded.push(b' ');
    assert!(crate::control::transport::decode(&padded).is_err());
}
