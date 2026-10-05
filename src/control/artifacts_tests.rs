use super::*;
use crate::control::{
    artifacts::{self, CHUNK_BYTES, Chunk},
    node::NodeStore,
    worker::{self, WorkerRequest},
};

pub(super) fn fixture() -> (Fixture, Attempt, String, NodeStore) {
    let mut f = Fixture::new();
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let mut c = contract();
    c.kind = "artifact".into();
    let task = text(
        &call(
            &mut f.store,
            &f.supervisor,
            Mutation::CreateTask {
                run: f.run.clone(),
                expected_revision: r.revision,
                contract: c,
                dependencies: vec![],
            },
        ),
        "id",
    );
    let (a, token) = f.dispatch(&task);
    f.ready(&a, &token);
    let mut node = node_store(&f);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let execute = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|x| x.kind == "execute")
        .unwrap();
    node.receive(&f.store.prepare_wire(&execute.id, &owner).unwrap())
        .unwrap();
    (f, a, token, node)
}
pub(super) fn chunks(name: &str, bytes: &[u8]) -> Vec<Chunk> {
    let sha256 = digest(bytes);
    (0..bytes.len().div_ceil(CHUNK_BYTES).max(1))
        .map(|index| {
            let begin = index * CHUNK_BYTES;
            Chunk {
                name: name.into(),
                sha256: sha256.clone(),
                size: bytes.len() as u64,
                index: index as u32,
                data: bytes[begin..bytes.len().min(begin + CHUNK_BYTES)].to_vec(),
            }
        })
        .collect()
}
pub(super) fn put(token: &str, a: &Attempt, chunk: Chunk) -> Request {
    request(
        token,
        Mutation::PutArtifact {
            attempt: a.id.clone(),
            epoch: a.epoch,
            chunk,
        },
    )
}
pub(super) fn result(token: &str, a: &Attempt, artifacts: BTreeMap<String, String>) -> Request {
    request(
        token,
        Mutation::SubmitResult {
            attempt: a.id.clone(),
            epoch: a.epoch,
            contract_hash: a.contract_hash.clone(),
            commit: None,
            artifacts,
            summary: "Artifact evidence".into(),
        },
    )
}
fn count(c: &rusqlite::Connection, table: &str) -> u64 {
    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn artifact_result_requires_verified_bytes_and_seals_exact_attempt_contract_and_manifest() {
    let (mut f, a, token, _node) = fixture();
    let bytes = (0..CHUNK_BYTES + 79)
        .map(|i| (i % 256) as u8)
        .collect::<Vec<_>>();
    let parts = chunks("report.bin", &bytes);
    let files = BTreeMap::from([("report.bin".into(), digest(&bytes))]);
    let submit = result(&token, &a, files.clone());
    assert!(f.store.apply(&submit).is_err());
    assert_eq!(count(&f.store.connection, "results"), 0);
    let tail = f.store.apply(&put(&token, &a, parts[1].clone())).unwrap();
    assert_eq!(tail["content_verified"], false);
    assert!(f.store.apply(&submit).is_err());
    let head = f.store.apply(&put(&token, &a, parts[0].clone())).unwrap();
    let audit: String = f
        .store
        .connection
        .query_row(
            "SELECT doc FROM journal WHERE kind='put_artifact' ORDER BY sequence DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert!(audit["params"]["chunk"].get("data").is_none());
    assert_eq!(
        audit["params"]["chunk"]["chunk_sha256"],
        digest(&parts[0].data)
    );

    assert_eq!(head["content_verified"], true);
    assert_eq!(head["task_accepted"], false);
    let mut synthetic = result(&token, &a, files);
    if let Mutation::SubmitResult { commit, .. } = &mut synthetic.command {
        *commit = Some(BASE.into());
    }
    assert!(f.store.apply(&synthetic).is_err());
    let accepted = f.store.apply(&submit).unwrap();
    assert_eq!(accepted["commit"], Value::Null);
    assert_eq!(accepted["artifact_manifest"]["attempt"], a.id);
    assert_eq!(
        accepted["artifact_manifest"]["contract_hash"],
        a.contract_hash
    );
    assert_eq!(
        accepted["artifact_manifest"]["files"]["report.bin"]["size"],
        bytes.len()
    );
    assert_eq!(
        accepted["artifact_manifest_hash"],
        digest(&serde_json::to_vec(&accepted["artifact_manifest"]).unwrap())
    );
    assert_eq!(f.store.apply(&submit).unwrap(), accepted);
    assert!(
        f.store
            .apply(&put(&token, &a, chunks("other.txt", b"later").remove(0)))
            .is_err()
    );
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &a.task)
            .unwrap()
            .state,
        "awaiting_review"
    );
    assert_eq!(
        f.store.snapshot(&f.owner, &f.run).unwrap()["active_attempts"],
        1
    );
}

#[test]
fn artifact_chunks_are_immutable_replayable_and_finalization_rolls_back_with_journal() {
    let (mut f, a, token, _node) = fixture();
    let bytes = vec![7; CHUNK_BYTES + 12];
    let parts = chunks("report", &bytes);
    let first = put(&token, &a, parts[0].clone());
    let response = f.store.apply(&first).unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(f.store.apply(&first).unwrap(), response);
    let mut corrupt = parts[0].clone();
    corrupt.data[0] ^= 1;
    assert!(f.store.apply(&put(&token, &a, corrupt)).is_err());
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 1);
    f.store.connection.execute_batch("CREATE TRIGGER artifact_no_journal BEFORE INSERT ON journal WHEN NEW.kind='put_artifact' BEGIN SELECT RAISE(ABORT,'receipt lost'); END;").unwrap();
    let second = put(&token, &a, parts[1].clone());
    assert!(f.store.apply(&second).is_err());
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 1);
    assert!(
        !f.store
            .connection
            .query_row("SELECT complete FROM artifact_uploads", [], |r| r
                .get::<_, bool>(0))
            .unwrap()
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER artifact_no_journal;")
        .unwrap();
    assert_eq!(f.store.apply(&second).unwrap()["content_verified"], true);
    let mut collision = parts[1].clone();
    collision.sha256 = digest(b"different");
    assert!(f.store.apply(&put(&token, &a, collision)).is_err());
    // Metadata marked complete must not hide corruption in retained bytes.
    f.store
        .connection
        .execute(
            "UPDATE artifact_chunks SET data=?1 WHERE chunk_index=1",
            [vec![8; 12]],
        )
        .unwrap();
    assert!(
        f.store
            .apply(&result(
                &token,
                &a,
                BTreeMap::from([("report".into(), digest(&bytes))])
            ))
            .is_err()
    );
    assert_eq!(count(&f.store.connection, "results"), 0);
}

#[test]
fn artifact_hash_mismatch_and_empty_files_have_no_false_completion() {
    let (mut f, a, token, _node) = fixture();
    let mut wrong = chunks("bad", b"original").remove(0);
    wrong.data = b"modified".to_vec();
    assert!(f.store.apply(&put(&token, &a, wrong)).is_err());
    assert_eq!(count(&f.store.connection, "artifact_uploads"), 0);
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 0);
    let empty = chunks("empty.txt", b"").remove(0);
    assert_eq!(
        f.store.apply(&put(&token, &a, empty)).unwrap()["content_verified"],
        true
    );
    let accepted = f
        .store
        .apply(&result(
            &token,
            &a,
            BTreeMap::from([("empty.txt".into(), digest(b""))]),
        ))
        .unwrap();
    assert_eq!(
        accepted["artifact_manifest"]["files"]["empty.txt"]["size"],
        0
    );
}

#[test]
fn artifact_scope_and_epoch_checks_apply_at_node_and_authority() {
    let (mut f, a, token, mut node) = fixture();
    let chunk = chunks("proof", b"private upload").remove(0);
    for secret in [&f.owner, &f.supervisor, &f.node] {
        assert!(f.store.apply(&put(secret, &a, chunk.clone())).is_err());
    }
    let mut stale = a.clone();
    stale.epoch += 1;
    assert!(f.store.apply(&put(&token, &stale, chunk.clone())).is_err());
    let req = put(&token, &a, chunk.clone());
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: "wrong".into(),
                operation_id: req.operation_id.clone(),
                command: Box::new(req.command.clone())
            }
        )
        .is_err()
    );
    let mut wrong = chunk.clone();
    wrong.data.push(0);
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: token.clone(),
                operation_id: id(),
                command: Box::new(Mutation::PutArtifact {
                    attempt: a.id.clone(),
                    epoch: 1,
                    chunk: wrong
                })
            }
        )
        .is_err()
    );
    assert_eq!(count(&node.connection, "events"), 0);
    node.connection
        .execute(
            "UPDATE jobs SET doc=json_set(doc,'$.attempt.role','reviewer')",
            [],
        )
        .unwrap();
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: token.clone(),
                operation_id: req.operation_id.clone(),
                command: Box::new(req.command.clone())
            }
        )
        .is_err()
    );
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: run.revision,
        },
    );
    assert!(f.store.apply(&req).is_err());
    assert_eq!(count(&f.store.connection, "artifact_uploads"), 0);
}

#[test]
fn artifact_shape_paths_and_reserved_capacity_are_bounded() {
    let (mut f, a, token, _node) = fixture();
    let sample = chunks("valid.txt", b"x").remove(0);
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "nested/file",
        "a\\b",
        ".git",
        "-option",
    ] {
        let mut chunk = sample.clone();
        chunk.name = name.into();
        assert!(f.store.apply(&put(&token, &a, chunk)).is_err());
    }
    for (size, index) in [
        (u64::MAX, 0),
        (artifacts::MAX_FILE_BYTES + 1, 0),
        (1, u32::MAX),
        (0, 1),
    ] {
        let mut chunk = sample.clone();
        chunk.size = size;
        chunk.index = index;
        assert!(f.store.apply(&put(&token, &a, chunk)).is_err());
    }
    let mut uppercase = sample.clone();
    uppercase.sha256 = uppercase.sha256.to_uppercase();
    assert!(f.store.apply(&put(&token, &a, uppercase)).is_err());
    for index in 0..4 {
        let chunk = Chunk {
            name: format!("file-{index}"),
            sha256: digest(b"claimed large file"),
            size: artifacts::MAX_FILE_BYTES,
            index: 0,
            data: vec![0; CHUNK_BYTES],
        };
        assert_eq!(
            f.store.apply(&put(&token, &a, chunk)).unwrap()["content_verified"],
            false
        );
    }
    assert!(f.store.apply(&put(&token, &a, sample)).is_err());
    assert_eq!(count(&f.store.connection, "artifact_uploads"), 4);
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 4);
    // Quota reserves declared total sizes, even when only one chunk has arrived.
    let (mut f, a, token, _node) = fixture();
    for index in 0..artifacts::MAX_FILES {
        assert_eq!(
            f.store
                .apply(&put(
                    &token,
                    &a,
                    chunks(&format!("empty-{index}"), b"").remove(0)
                ))
                .unwrap()["content_verified"],
            true
        );
    }
    assert!(
        f.store
            .apply(&put(&token, &a, chunks("extra", b"").remove(0)))
            .is_err()
    );
}

#[test]
fn concurrent_artifact_chunks_commit_one_complete_content_and_manifest() {
    let (f, a, token, _node) = fixture();
    let bytes = (0..CHUNK_BYTES * 4 + 1)
        .map(|x| (x % 251) as u8)
        .collect::<Vec<_>>();
    let requests = chunks("parallel.bin", &bytes)
        .into_iter()
        .map(|c| put(&token, &a, c))
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(requests.len() * 2));
    let mut threads = Vec::new();
    for req in requests.iter().chain(requests.iter()) {
        let barrier = barrier.clone();
        let path = f.store.dir.clone();
        let req = Request {
            token: req.token.clone(),
            operation_id: req.operation_id.clone(),
            command: req.command.clone(),
        };
        threads.push(std::thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            store.apply(&req).unwrap()
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(
        count(&f.store.connection, "artifact_chunks"),
        requests.len() as u64
    );
    artifacts::verify(
        &f.store.connection,
        &a.id,
        "parallel.bin",
        &artifacts::File {
            sha256: digest(&bytes),
            size: bytes.len() as u64,
        },
    )
    .unwrap();
    let manifest = artifacts::seal(
        &f.store.connection,
        &a,
        &BTreeMap::from([("parallel.bin".into(), digest(&bytes))]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(manifest.attempt, a.id);
}

#[tokio::test]
async fn real_nats_artifact_chunks_recover_result_before_content_across_restart() {
    use crate::control::{
        broker_policy::Endpoint,
        transport::{Broker, controller_once, node_once},
    };
    let (mut f, a, token, mut node) = fixture();
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
    let broker = Broker::connect(&server.config("node"), &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    let bytes = vec![255; CHUNK_BYTES + 37];
    let hash = digest(&bytes);
    let submit = result(
        &token,
        &a,
        BTreeMap::from([("wire.bin".into(), hash.clone())]),
    );
    node.enqueue(&submit, &f.node_key).unwrap();
    for chunk in chunks("wire.bin", &bytes).into_iter().rev() {
        let req = put(&token, &a, chunk);
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: token.clone(),
                operation_id: req.operation_id,
                command: Box::new(req.command),
            },
        )
        .unwrap();
    }
    let pending = node.pending().unwrap();
    assert!(
        pending
            .iter()
            .all(|(_, wire)| serde_json::to_vec(wire).unwrap().len() < 300_000)
    );
    node_once(&mut node, &broker).await.unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    assert_eq!(count(&f.store.connection, "results"), 0);
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 2);
    drop(node);
    let mut node = node_store(&f);
    node_once(&mut node, &broker).await.unwrap();
    let status = worker::handle(
        &mut node,
        &f.node_key,
        WorkerRequest::ArtifactStatus {
            token: token.clone(),
            attempt: a.id.clone(),
            epoch: 1,
            name: "wire.bin".into(),
            sha256: hash,
        },
    )
    .unwrap();
    assert_eq!(status["content_verified"], true);
    assert_eq!(status["applied_chunks"], 2);
    assert_eq!(status["task_accepted"], false);
    assert!(!status.to_string().contains(&token));
    f.store = Store::open(&f.store.dir).unwrap();
    f.store
        .connection
        .execute(
            "UPDATE reception_failures SET available_at=0 WHERE state='pending'",
            [],
        )
        .unwrap();
    controller_once(&mut f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    assert_eq!(count(&f.store.connection, "results"), 1);
    // Replay accepted records even after the immutable result retired upload mutation.
    for (_, wire) in pending {
        assert!(f.store.receive(&wire).is_ok());
    }
    assert_eq!(count(&f.store.connection, "artifact_chunks"), 2);
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &a.task)
            .unwrap()
            .state,
        "awaiting_review"
    );
}

#[test]
fn controller_artifact_schema_upgrade_preserves_prior_authority_and_frozen_records() {
    let (mut f, a, _token, _node) = fixture();
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
            "DROP TABLE integration_policies; DROP TABLE attempt_profiles; DROP TABLE artifact_chunks; DROP TABLE artifact_uploads; DROP TABLE registration_recoveries; PRAGMA user_version=6;",
        )
        .unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(
        f.store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_eq!(
        document::<Attempt>(&f.store.connection, "attempts", &a.id)
            .unwrap()
            .state,
        "running"
    );
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT wire FROM outbox WHERE kind='execute'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        before
    );
    assert_eq!(count(&f.store.connection, "artifact_uploads"), 0);
}

#[test]
fn artifact_status_remains_readable_during_execution_fence_and_rejects_retired_authority() {
    let (f, a, token, mut node) = fixture();
    let lease = crate::control::api::lock(&node.dir, "execution.lock").unwrap();
    let read = || WorkerRequest::ArtifactStatus {
        token: token.clone(),
        attempt: a.id.clone(),
        epoch: 1,
        name: "report".into(),
        sha256: digest(b"data"),
    };
    assert_eq!(
        worker::handle(&mut node, &f.node_key, read()).unwrap()["queued_chunks"],
        0
    );
    let req = put(&token, &a, chunks("report", b"data").remove(0));
    assert!(
        worker::handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Apply {
                token: token.clone(),
                operation_id: req.operation_id,
                command: Box::new(req.command)
            }
        )
        .is_err()
    );
    drop(lease);
    node.connection
        .execute("UPDATE jobs SET epoch=2,state='cancelling'", [])
        .unwrap();
    assert!(worker::handle(&mut node, &f.node_key, read()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_file_upload_retries_busy_ipc_resumes_exact_events_and_rejects_symlinks() {
    let (f, a, token, node) = fixture();
    f.node_key.save_new(&node.dir.join("node.key")).unwrap();
    let state = node.dir.clone();
    let access = worker::Access {
        state: state.clone(),
        node: "local".into(),
        owner: f.store.owner().unwrap(),
        grant: node.grant.clone(),
    };
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        worker::serve(access, async {
            let _ = stop.await;
        })
        .await
    });
    for _ in 0..100 {
        if state.join("worker.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(state.join("worker.sock").exists());
    let path = f.dir.path().join("source.bin");
    let bytes = vec![197; CHUNK_BYTES + 3];
    std::fs::write(&path, &bytes).unwrap();
    let result = tokio::task::spawn_blocking(move || {
        let lease = crate::control::api::lock(&state, "execution.lock").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(lease);
        });
        let first = artifacts::upload(&state, &token, &a.id, 1, "report.bin", &path).unwrap();
        release.join().unwrap();
        assert_eq!(
            artifacts::upload(&state, &token, &a.id, 1, "report.bin", &path).unwrap(),
            first
        );
        assert_eq!(first["events"].as_array().unwrap().len(), 2);
        assert_eq!(first["sha256"], digest(&bytes));
        assert!(!first.to_string().contains(&token));
        let link = path.with_extension("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(artifacts::upload(&state, &token, &a.id, 1, "report.bin", &link).is_err());
        let large = path.with_extension("large");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(artifacts::MAX_FILE_BYTES + 1)
            .unwrap();
        assert!(artifacts::upload(&state, &token, &a.id, 1, "report.bin", &large).is_err());
        first
    })
    .await;
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
    result.unwrap();
    assert_eq!(count(&node.connection, "events"), 2);
}

#[test]
fn artifact_receipt_backlog_cannot_delay_due_cancellation() {
    let (mut f, _a, _token, _node) = fixture();
    for index in 0..80 {
        effect(
            &f.store.connection,
            "local",
            "receipt",
            &json!({"fixture":index}),
            now() + 3600,
        )
        .unwrap();
    }
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: run.revision,
        },
    );
    let due = crate::control::delivery::controller_due(&f.store.connection).unwrap();
    assert_eq!(due.len(), 16);
    let kind: String = f
        .store
        .connection
        .query_row("SELECT kind FROM outbox WHERE id=?1", [&due[0].0], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "cancel");
    assert!(
        f.store
            .connection
            .query_row(
                "SELECT count(*) FROM outbox WHERE kind='receipt' AND state='pending'",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap()
            > 64
    );
}
