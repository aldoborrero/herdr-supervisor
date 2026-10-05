use super::*;
use crate::control::{
    artifact_review,
    artifact_review_retirement::{self as retirement, Request as Decision},
    artifacts::{Candidate, Chunk, File, Manifest},
    model::ReviewContext,
    node::ExecuteOrder,
};
use rusqlite::params;

struct Review {
    s: Setup,
    order: ExecuteOrder,
    candidate: Candidate,
    path: PathBuf,
}
impl Review {
    fn new() -> Self {
        let mut s = Setup::new();
        let doc: String = s
            .node
            .connection
            .query_row("SELECT doc FROM jobs WHERE id=?1", [&s.attempt.id], |r| {
                r.get(0)
            })
            .unwrap();
        let mut order: ExecuteOrder = serde_json::from_str(&doc).unwrap();
        order.attempt.role = "reviewer".into();
        let bytes = b"Retained artifact bytes";
        let manifest = Manifest {
            attempt: id(),
            contract_hash: digest(b"sealed source contract"),
            files: BTreeMap::from([(
                "evidence.txt".into(),
                File {
                    sha256: digest(bytes),
                    size: bytes.len() as u64,
                },
            )]),
            version: 1,
        };
        let candidate = Candidate {
            manifest_hash: digest(&serde_json::to_vec(&manifest).unwrap()),
            manifest,
        };
        order.review = Some(ReviewContext {
            artifact: Some(candidate.clone()),
            integration: id(),
            candidate_oid: String::new(),
            base_oid: s.plan.base_oid.clone(),
            result_oid: String::new(),
            checks_digest: digest(b"checks"),
        });
        s.node
            .connection
            .execute(
                "UPDATE jobs SET doc=?1 WHERE id=?2",
                params![serde_json::to_string(&order).unwrap(), s.attempt.id],
            )
            .unwrap();
        assert!(
            artifact_review::stage(&mut s.node, &order, &s.plan.worktree, &s.f.node_key)
                .unwrap()
                .is_none()
        );
        for (event, wire) in s.node.pending().unwrap() {
            let req: Request = serde_json::from_value(wire.record.payload).unwrap();
            let Mutation::FetchArtifactChunk {
                attempt,
                integration,
                name,
                index,
                ..
            } = req.command
            else {
                panic!("unexpected fixture event")
            };
            let response = json!({"id":attempt,"integration":integration,"manifest_hash":candidate.manifest_hash,"chunk":Chunk{name,sha256:digest(bytes),size:bytes.len() as u64,index,data:bytes.to_vec()}});
            s.node
                .connection
                .execute(
                    "UPDATE events SET state='controller_applied',response=?1 WHERE id=?2",
                    params![response.to_string(), event],
                )
                .unwrap();
        }
        let path = artifact_review::stage(&mut s.node, &order, &s.plan.worktree, &s.f.node_key)
            .unwrap()
            .unwrap();
        s.stop();
        Self {
            s,
            order,
            candidate,
            path,
        }
    }
    fn decision(&self) -> Decision {
        Decision {
            operation: id(),
            attempt: self.s.attempt.id.clone(),
            expected_epoch: self.s.attempt.epoch,
            expected_manifest_hash: self.candidate.manifest_hash.clone(),
            processes_confirmed_stopped: true,
            retain_receipts_remove_inputs: true,
            reason: "Isolated fixture stopped; retain received bytes".into(),
        }
    }
    fn retire(&self, r: &Decision) -> anyhow::Result<Value> {
        retirement::retire_with(&self.s.node.dir, &self.s.config, r, &self.s.native)
    }
    fn count(&self) -> i64 {
        self.s
            .node
            .connection
            .query_row("SELECT count(*) FROM artifact_copy_retirements", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn cached(&self) -> (String, String) {
        self.s.node.connection.query_row("SELECT id,response FROM events WHERE json_extract(wire,'$.record.payload.command.method')='fetch_artifact_chunk'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap()
    }
}

#[test]
fn reviewer_copy_retirement_preserves_receipts_and_other_files_then_permits_checkout_retirement() {
    let mut x = Review::new();
    let r = x.decision();
    let cached = x.cached();
    let other = x.s.plan.worktree.join("unsaved.txt");
    std::fs::write(&other, "user data").unwrap();
    let receipt = x.retire(&r).unwrap();
    assert_eq!(receipt["state"], "removed");
    assert_eq!(receipt["worktree_removed"], false);
    assert!(!x.path.exists());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "user data");
    assert_eq!(x.cached(), cached);
    artifact_review::verify_cached(&x.s.node.connection, &x.order, &x.candidate).unwrap();
    assert!(
        artifact_review::stage(&mut x.s.node, &x.order, &x.s.plan.worktree, &x.s.f.node_key)
            .is_err()
    );
    assert!(!x.path.exists());
    x.s.node = node_store(&x.s.f);
    assert_eq!(x.retire(&r).unwrap(), receipt);
    assert_eq!(
        x.s.node
            .connection
            .query_row("SELECT state FROM artifact_staging", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "retired"
    );
    x.s.native.removal.set(1);
    let cleanup = crate::control::retirement::Request {
        operation: id(),
        attempt: r.attempt.clone(),
        expected_epoch: r.expected_epoch,
        expected_head: x.s.plan.base_oid.clone(),
        processes_confirmed_stopped: true,
        retain_commits_remove_checkout: true,
        reason: "Fixture stopped; preserve commits".into(),
    };
    assert!(
        crate::control::retirement::retire_with(&x.s.node.dir, &x.s.config, &cleanup, &x.s.native)
            .is_err()
    );
    std::fs::remove_file(other).unwrap();
    assert_eq!(
        crate::control::retirement::retire_with(&x.s.node.dir, &x.s.config, &cleanup, &x.s.native)
            .unwrap()["state"],
        "removed"
    );
    assert_eq!(x.retire(&r).unwrap(), receipt);
    assert_eq!(x.cached(), cached);
}

#[test]
fn reviewer_copy_retirement_requires_confirmed_stop_exit_scope_and_complete_cached_bytes() {
    let x = Review::new();
    let mut r = x.decision();
    r.processes_confirmed_stopped = false;
    assert!(x.retire(&r).is_err());
    r.processes_confirmed_stopped = true;
    r.retain_receipts_remove_inputs = false;
    assert!(x.retire(&r).is_err());
    r.retain_receipts_remove_inputs = true;
    r.expected_epoch += 1;
    assert!(x.retire(&r).is_err());
    r.expected_epoch -= 1;
    x.s.node
        .connection
        .execute("UPDATE jobs SET state='running'", [])
        .unwrap();
    assert!(x.retire(&r).is_err());
    x.s.node
        .connection
        .execute("UPDATE jobs SET state='stopped'", [])
        .unwrap();
    let (event, response) = x.cached();
    x.s.node
        .connection
        .execute(
            "UPDATE events SET state='broker_stored' WHERE id=?1",
            [&event],
        )
        .unwrap();
    assert!(x.retire(&r).is_err());
    x.s.node
        .connection
        .execute(
            "UPDATE events SET state='controller_applied' WHERE id=?1",
            [&event],
        )
        .unwrap();
    let mut changed: Value = serde_json::from_str(&response).unwrap();
    changed["chunk"]["data"][0] = json!(0);
    x.s.node
        .connection
        .execute(
            "UPDATE events SET response=?1 WHERE id=?2",
            params![changed.to_string(), event],
        )
        .unwrap();
    assert!(x.retire(&r).is_err());
    changed = serde_json::from_str(&response).unwrap();
    changed["integration"] = json!(id());
    x.s.node
        .connection
        .execute(
            "UPDATE events SET response=?1 WHERE id=?2",
            params![changed.to_string(), event],
        )
        .unwrap();
    assert!(x.retire(&r).is_err());
    x.s.node
        .connection
        .execute(
            "UPDATE events SET response=?1 WHERE id=?2",
            params![response, event],
        )
        .unwrap();
    x.s.node
        .connection
        .execute(
            "INSERT INTO runtime_processes VALUES (?1,?2,'running','{}')",
            params![r.attempt, r.expected_epoch],
        )
        .unwrap();
    assert!(x.retire(&r).is_err());
    x.s.node
        .connection
        .execute(
            "UPDATE runtime_processes SET state='exited',doc=?1",
            [
                json!({"attempt":id(),"epoch":r.expected_epoch,"scope":"native_process"})
                    .to_string(),
            ],
        )
        .unwrap();
    assert!(x.retire(&r).is_err());
    assert_eq!(x.count(), 0);
    assert!(x.path.join("evidence.txt").exists());
}

#[test]
fn reviewer_copy_retirement_refuses_foreign_native_resources_extra_data_and_changed_authority() {
    let mut x = Review::new();
    let r = x.decision();
    x.s.native.terminal.set(2);
    assert!(x.retire(&r).is_err());
    x.s.native.terminal.set(1);
    x.s.native.foreground.set(false);
    assert!(x.retire(&r).is_err());
    x.s.native.foreground.set(true);
    let socket = x.s.config.herdr_socket.clone();
    x.s.config.herdr_socket = "/foreign.sock".into();
    assert!(x.retire(&r).is_err());
    x.s.config.herdr_socket = socket;
    std::fs::write(x.path.join("notes"), "keep").unwrap();
    assert!(x.retire(&r).is_err());
    std::fs::remove_file(x.path.join("notes")).unwrap();
    x.s.native.change_database = Some(x.s.node.dir.join("node.sqlite"));
    assert!(x.retire(&r).is_err());
    assert_eq!(x.count(), 0);
    assert!(x.path.join("evidence.txt").exists());
}

#[test]
fn reviewer_copy_retirement_receipt_failure_is_atomic_and_recreated_data_is_retained() {
    let mut x = Review::new();
    let r = x.decision();
    x.s.node.connection.execute_batch("CREATE TRIGGER lose_intent BEFORE INSERT ON artifact_copy_retirements BEGIN SELECT RAISE(FAIL,'lost intent'); END;").unwrap();
    assert!(x.retire(&r).is_err());
    assert_eq!(x.count(), 0);
    assert!(x.path.join("evidence.txt").exists());
    x.s.node.connection.execute_batch("DROP TRIGGER lose_intent; CREATE TRIGGER lose_receipt BEFORE UPDATE ON artifact_copy_retirements WHEN NEW.state='removed' BEGIN SELECT RAISE(FAIL,'lost receipt'); END;").unwrap();
    assert!(x.retire(&r).is_err());
    assert!(!x.path.exists());
    // Input deletion with a lost SQL receipt is not permission to remove the
    // surrounding checkout before its staging ledger has been reconciled.
    x.s.native.removal.set(1);
    let checkout = crate::control::retirement::Request {
        operation: id(),
        attempt: r.attempt.clone(),
        expected_epoch: r.expected_epoch,
        expected_head: x.s.plan.base_oid.clone(),
        processes_confirmed_stopped: true,
        retain_commits_remove_checkout: true,
        reason: "Fixture staged receipt still unresolved".into(),
    };
    assert!(
        crate::control::retirement::retire_with(&x.s.node.dir, &x.s.config, &checkout, &x.s.native)
            .is_err()
    );
    assert_eq!(x.s.native.remove_calls.get(), 0);
    assert_eq!(
        x.s.node
            .connection
            .query_row("SELECT state FROM artifact_staging", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "ready"
    );
    x.s.node
        .connection
        .execute_batch("DROP TRIGGER lose_receipt")
        .unwrap();
    private_dir(&x.path).unwrap();
    std::fs::write(x.path.join("new-data"), "keep").unwrap();
    assert!(x.retire(&r).is_err());
    assert_eq!(
        std::fs::read_to_string(x.path.join("new-data")).unwrap(),
        "keep"
    );
    std::fs::remove_file(x.path.join("new-data")).unwrap();
    std::fs::remove_dir(&x.path).unwrap();
    x.s.node = node_store(&x.s.f);
    assert_eq!(x.retire(&r).unwrap()["state"], "removed");
    let mut changed = x.decision();
    changed.operation = r.operation;
    changed.reason.push('!');
    assert!(x.retire(&changed).is_err());
}

#[test]
fn reviewer_copy_retirement_migration_preserves_staging_and_receipts_and_fences_executor() {
    let mut x = Review::new();
    let r = x.decision();
    let cached = x.cached();
    x.s.node.connection.execute_batch("DROP TABLE artifact_copy_retirements; ALTER TABLE artifact_staging RENAME TO latest_staging; CREATE TABLE artifact_staging (attempt TEXT PRIMARY KEY REFERENCES jobs(id),epoch INTEGER NOT NULL,manifest_hash TEXT NOT NULL,path TEXT NOT NULL UNIQUE,state TEXT NOT NULL CHECK(state IN ('creating','ready'))) STRICT; INSERT INTO artifact_staging SELECT * FROM latest_staging; DROP TABLE latest_staging; PRAGMA user_version=10;").unwrap();
    x.s.node = node_store(&x.s.f);
    assert_eq!(x.cached(), cached);
    assert!(x.path.join("evidence.txt").exists());
    let lease = crate::control::api::lock(&x.s.node.dir, "execution.lock").unwrap();
    assert!(x.retire(&r).is_err());
    assert_eq!(x.count(), 0);
    drop(lease);
    assert_eq!(x.retire(&r).unwrap()["state"], "removed");
    assert_eq!(x.cached(), cached);
}
