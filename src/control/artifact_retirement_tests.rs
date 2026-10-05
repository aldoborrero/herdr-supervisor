use super::*;
use crate::control::{
    artifact_removal,
    artifact_retirement::{self, Request},
};

fn terminal() -> Artifacts {
    let mut x = Artifacts::new("true", false);
    assert_eq!(x.advance().unwrap().state, "verified");
    x.apply();
    assert_eq!(x.advance().unwrap().state, "applied");
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::StopAttempt {
            attempt: x.author.id.clone(),
            expected_epoch: x.author.epoch,
        },
    );
    let stopped: Attempt = document(&x.f.store.connection, "attempts", &x.author.id).unwrap();
    call(
        &mut x.f.store,
        &x.f.node,
        Mutation::ObserveStopped {
            attempt: stopped.id,
            epoch: stopped.epoch,
        },
    );
    let r: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::CompleteRun {
            run: r.id,
            expected_revision: r.revision,
        },
    );
    x
}
fn decision(x: &Artifacts) -> Request {
    let i = verification::integration(&x.f.store.connection, &x.integration.id).unwrap();
    Request {
        operation: id(),
        integration: i.id,
        expected_revision: i.revision,
        expected_manifest_hash: i.artifact.unwrap().manifest_hash,
        processes_confirmed_stopped: true,
        retain_source_remove_inputs: true,
        reason: "All fixture checks stopped; keep original SQLite content".into(),
    }
}
fn retire(x: &mut Artifacts, r: &Request) -> anyhow::Result<Value> {
    artifact_retirement::retire(&mut x.f.store, &x.f.owner, r)
}

#[test]
fn artifact_retirement_removes_only_copies_and_preserves_source_evidence_and_replay() {
    let mut x = terminal();
    let r = decision(&x);
    let candidate = x.integration.artifact.clone().unwrap();
    let original =
        crate::control::artifacts::content(&x.f.store.connection, &candidate, "report.txt")
            .unwrap();
    let receipt = retire(&mut x, &r).unwrap();
    assert_eq!(receipt["state"], "removed");
    assert_eq!(receipt["source_retained"], true);
    assert!(!x.inputs().exists());
    assert_eq!(
        crate::control::artifacts::content(&x.f.store.connection, &candidate, "report.txt")
            .unwrap(),
        original
    );
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM check_runs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT state FROM verification_worktrees", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "retired"
    );
    assert_eq!(retire(&mut x, &r).unwrap(), receipt);
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(retire(&mut x, &r).unwrap(), receipt);
    assert!(artifact_retirement::retire(&mut x.f.store, &x.f.supervisor, &r).is_err());
    assert!(x.advance().is_err());
    assert_eq!(
        verification::integration(&x.f.store.connection, &x.integration.id)
            .unwrap()
            .state,
        "applied"
    );
    assert!(!x.inputs().exists());
}

#[test]
fn artifact_retirement_requires_terminal_scope_exact_manifest_and_complete_original() {
    let mut x = terminal();
    let mut r = decision(&x);
    r.processes_confirmed_stopped = false;
    assert!(retire(&mut x, &r).is_err());
    r.processes_confirmed_stopped = true;
    r.retain_source_remove_inputs = false;
    assert!(retire(&mut x, &r).is_err());
    r.retain_source_remove_inputs = true;
    r.expected_revision += 1;
    assert!(retire(&mut x, &r).is_err());
    r.expected_revision -= 1;
    let hash = r.expected_manifest_hash.clone();
    r.expected_manifest_hash = "f".repeat(64);
    assert!(retire(&mut x, &r).is_err());
    r.expected_manifest_hash = hash;
    let mut run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    let state = run.state.clone();
    run.state = "active".into();
    save_run(&x.f.store.connection, &run).unwrap();
    assert!(retire(&mut x, &r).is_err());
    run.state = state;
    save_run(&x.f.store.connection, &run).unwrap();
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='started'", [])
        .unwrap();
    assert!(retire(&mut x, &r).is_err());
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='passed'", [])
        .unwrap();
    let original: Vec<u8> =
        x.f.store
            .connection
            .query_row("SELECT data FROM artifact_chunks", [], |r| r.get(0))
            .unwrap();
    x.f.store
        .connection
        .execute("UPDATE artifact_chunks SET data=X'ff'", [])
        .unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(x.inputs().join("report.txt").exists());
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM artifact_retirements", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    x.f.store
        .connection
        .execute("UPDATE artifact_chunks SET data=?1", [original])
        .unwrap();
    assert_eq!(retire(&mut x, &r).unwrap()["state"], "removed");
}

#[test]
fn artifact_retirement_preserves_extra_changed_symlinked_and_unowned_marker_data() {
    let mut x = terminal();
    let r = decision(&x);
    let inputs = x.inputs();
    std::fs::write(inputs.join("notes"), b"keep").unwrap();
    assert!(retire(&mut x, &r).is_err());
    std::fs::remove_file(inputs.join("notes")).unwrap();
    let bytes = std::fs::read(inputs.join("report.txt")).unwrap();
    std::fs::remove_file(inputs.join("report.txt")).unwrap();
    std::fs::write(inputs.join("report.txt"), b"modified input").unwrap();
    assert!(retire(&mut x, &r).is_err());
    std::fs::remove_file(inputs.join("report.txt")).unwrap();
    let other = x.f.store.dir.join("outside");
    std::fs::write(&other, &bytes).unwrap();
    std::os::unix::fs::symlink(&other, inputs.join("report.txt")).unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert_eq!(std::fs::read(&other).unwrap(), bytes);
    std::fs::remove_file(inputs.join("report.txt")).unwrap();
    std::fs::write(inputs.join("report.txt"), &bytes).unwrap();
    std::fs::write(inputs.join(".herdr-supervisor-retirement"), b"foreign").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(inputs.join("report.txt").exists());
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM artifact_retirements", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn artifact_retirement_audit_failure_rolls_back_and_reconciles_without_recreating_inputs() {
    let mut x = terminal();
    let r = decision(&x);
    x.f.store.connection.execute_batch("CREATE TRIGGER fail_start BEFORE INSERT ON journal WHEN NEW.kind='artifact_retirement_prepared' BEGIN SELECT RAISE(FAIL,'lost audit'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(x.inputs().join("report.txt").exists());
    assert!(!x.inputs().join(".herdr-supervisor-retirement").exists());
    x.f.store.connection.execute_batch("DROP TRIGGER fail_start; CREATE TRIGGER fail_end BEFORE INSERT ON journal WHEN NEW.kind='artifact_retirement_completed' BEGIN SELECT RAISE(FAIL,'lost audit'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(!x.inputs().exists());
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT state FROM artifact_retirements", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "removing"
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_end")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(retire(&mut x, &r).unwrap()["state"], "removed");
    let mut changed = decision(&x);
    changed.operation = r.operation.clone();
    changed.reason.push('!');
    assert!(retire(&mut x, &changed).is_err());
    assert_eq!(
        x.f.store
            .connection
            .query_row(
                "SELECT count(*) FROM journal WHERE kind='artifact_retirement_completed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn artifact_retirement_preserves_recreated_directory_and_migrates_existing_results() {
    let mut x = terminal();
    let r = decision(&x);
    let before: String =
        x.f.store
            .connection
            .query_row("SELECT doc FROM results", [], |r| r.get(0))
            .unwrap();
    x.f.store
        .connection
        .execute_batch("DROP TABLE artifact_retirements; PRAGMA user_version=13")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT doc FROM results", [], |r| r.get::<_, String>(0))
            .unwrap(),
        before
    );
    let lease = crate::control::api::lock(&x.f.store.dir, "git-executor.lock").unwrap();
    assert!(retire(&mut x, &r).is_err());
    drop(lease);
    x.f.store.connection.execute_batch("CREATE TRIGGER fail_end BEFORE INSERT ON journal WHEN NEW.kind='artifact_retirement_completed' BEGIN SELECT RAISE(FAIL,'lost audit'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(!x.inputs().exists());
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_end")
        .unwrap();
    private_dir(&x.inputs()).unwrap();
    std::fs::write(x.inputs().join("new-data"), b"keep").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert_eq!(std::fs::read(x.inputs().join("new-data")).unwrap(), b"keep");
    std::fs::remove_file(x.inputs().join("new-data")).unwrap();
    assert!(retire(&mut x, &r).is_err());
    // Empty but unmarked is still retained; only separate operator action clears it.
    std::fs::remove_dir(x.inputs()).unwrap();
    assert_eq!(retire(&mut x, &r).unwrap()["state"], "removed");
}

#[test]
fn artifact_retirement_partial_removal_resumes_only_matching_marked_content() {
    let x = terminal();
    let path = x.inputs();
    let mut candidate = x.integration.artifact.clone().unwrap();
    let bytes = b"second file";
    candidate.manifest.files.insert(
        "second.txt".into(),
        crate::control::artifacts::File {
            sha256: digest(bytes),
            size: bytes.len() as u64,
        },
    );
    candidate.manifest_hash = digest(&serde_json::to_vec(&candidate.manifest).unwrap());
    std::fs::write(path.join("second.txt"), bytes).unwrap();
    let identity = artifact_removal::inspect(&path, &candidate).unwrap();
    let marker = b"exact-operation-marker";
    artifact_removal::prepare(&path, &candidate, &identity, marker).unwrap();
    assert!(
        artifact_removal::remove_with(&path, &candidate, &identity, marker, |_| anyhow::bail!(
            "injected crash after first unlink"
        ))
        .is_err()
    );
    assert!(!path.join("report.txt").exists());
    assert!(path.join("second.txt").exists());
    std::fs::write(path.join("foreign"), b"keep").unwrap();
    assert!(artifact_removal::remove(&path, &candidate, &identity, marker).is_err());
    assert!(path.join("second.txt").exists());
    std::fs::remove_file(path.join("foreign")).unwrap();
    std::fs::write(path.join("second.txt"), b"changed").unwrap();
    assert!(artifact_removal::remove(&path, &candidate, &identity, marker).is_err());
    std::fs::write(path.join("second.txt"), bytes).unwrap();
    let old = path.with_file_name("original-inputs");
    std::fs::rename(&path, &old).unwrap();
    private_dir(&path).unwrap();
    for name in ["second.txt", ".herdr-supervisor-retirement"] {
        std::fs::copy(old.join(name), path.join(name)).unwrap();
    }
    assert!(artifact_removal::remove(&path, &candidate, &identity, marker).is_err());
    assert!(path.join("second.txt").exists());
    for name in ["second.txt", ".herdr-supervisor-retirement"] {
        std::fs::remove_file(path.join(name)).unwrap();
    }
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(old, &path).unwrap();
    artifact_removal::remove(&path, &candidate, &identity, marker).unwrap();
    assert!(!path.exists());
}
