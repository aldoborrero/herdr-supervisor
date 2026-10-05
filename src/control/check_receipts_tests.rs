use super::*;
use crate::control::check_receipts;
use std::os::unix::fs::PermissionsExt;

fn pending(check: &str) -> (GitFixture, Integration, String, PathBuf) {
    let mut x = GitFixture::new("sha1", check, false);
    x.submit();
    let i = x.request();
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_finish BEFORE UPDATE ON check_runs BEGIN SELECT RAISE(ABORT,'lost final SQL receipt'); END;").unwrap();
    assert!(git_pipeline::advance(&mut x.f.store, &x.config, &i.id, &RealRunner).is_err());
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_finish;")
        .unwrap();
    let check: String =
        x.f.store
            .connection
            .query_row(
                "SELECT id FROM check_runs WHERE integration=?1",
                [&i.id],
                |r| r.get(0),
            )
            .unwrap();
    let path = check_receipts::path(&x.f.store.dir, &check).unwrap();
    assert!(path.is_file());
    (x, i, check, path)
}

fn state(x: &GitFixture) -> String {
    x.f.store
        .connection
        .query_row(
            "SELECT state FROM check_runs ORDER BY rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn check_receipt_recovers_finished_verifier_after_lost_sql_without_reexecuting() {
    let mut x = GitFixture::new("sha1", "printf x >> ../executions; true", false);
    x.submit();
    let i = x.request();
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_finish BEFORE UPDATE ON check_runs BEGIN SELECT RAISE(ABORT,'lost final SQL receipt'); END;").unwrap();
    assert!(git_pipeline::advance(&mut x.f.store, &x.config, &i.id, &RealRunner).is_err());
    let counter =
        x.f.store
            .dir
            .join("verification")
            .join(&i.id)
            .join("executions");
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "x");
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_finish;")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(x.advance(&i.id).state, "verified");
    assert_eq!(std::fs::read_to_string(counter).unwrap(), "x");
}

#[test]
fn check_receipt_signed_provenance_and_outcome_cannot_be_substituted() {
    let (mut x, _, check, path) = pending("true");
    let bytes = std::fs::read(&path).unwrap();
    let initial: String =
        x.f.store
            .connection
            .query_row("SELECT doc FROM check_runs WHERE id=?1", [&check], |r| {
                r.get(0)
            })
            .unwrap();
    let valid: Value = serde_json::from_slice(&bytes).unwrap();
    for (pointer, value) in [
        ("/owner", json!(Identity::generate().unwrap().did())),
        ("/initial_hash", json!("0".repeat(64))),
        ("/receipt/program", json!("/other/check")),
        ("/receipt/candidate", json!(BASE)),
        ("/receipt/argv", json!(["false"])),
        ("/receipt/output/exit_code", json!(7)),
        ("/receipt/output/stdout", json!("fabricated private text")),
        ("/attestation/signature", json!("bad")),
        ("/attestation/record/id", json!(id())),
    ] {
        let mut bad = valid.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        let status = check_receipts::reconcile(&mut x.f.store).unwrap();
        assert_eq!(status["recovered"], json!([]), "{pointer}");
        assert_eq!(state(&x), "started");
        assert!(!status.to_string().contains("fabricated private text"));
    }
    // These changes remain internally consistent, so only the owner's original
    // signed commitment prevents substituting a different result or log.
    let mut coherent = valid.clone();
    coherent["state"] = json!("failed");
    coherent["receipt"]["output"]["exit_code"] = json!(7);
    std::fs::write(&path, coherent.to_string()).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    let mut coherent = valid.clone();
    coherent["receipt"]["output"]["stdout"] = json!("substituted");
    coherent["receipt"]["output"]["stdout_hash"] = json!(digest(b"substituted"));
    std::fs::write(&path, coherent.to_string()).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    std::fs::write(&path, &bytes).unwrap();
    let mut changed: Value = serde_json::from_str(&initial).unwrap();
    changed["timeout_secs"] = json!(123);
    x.f.store
        .connection
        .execute(
            "UPDATE check_runs SET doc=?2 WHERE id=?1",
            rusqlite::params![check, changed.to_string()],
        )
        .unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    x.f.store
        .connection
        .execute(
            "UPDATE check_runs SET doc=?2 WHERE id=?1",
            rusqlite::params![check, initial],
        )
        .unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([check])
    );
    assert_eq!(state(&x), "passed");
}

#[test]
fn check_receipt_missing_partial_oversized_public_and_linked_files_never_become_results() {
    let (mut x, _, check, path) = pending("true");
    let bytes = std::fs::read(&path).unwrap();
    let backup = path.with_extension("retained");
    std::fs::rename(&path, &backup).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    assert!(!path.exists());
    for bad in [&bytes[..bytes.len() / 2], &vec![b' '; 1024 * 1024 + 1][..]] {
        secret_file(&path, bad).unwrap();
        assert_eq!(
            check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
            json!([])
        );
        assert_eq!(state(&x), "started");
        std::fs::remove_file(&path).unwrap();
    }
    std::os::unix::fs::symlink(&backup, &path).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    assert_eq!(std::fs::read(&backup).unwrap(), bytes);
    std::fs::remove_file(&path).unwrap();
    secret_file(&path, &bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([check])
    );
}

#[test]
fn check_receipt_failed_outcomes_and_dirty_checkout_evidence_remain_failed() {
    for command in [
        "printf x >> ../executions; exit 7",
        "printf changed >> base.txt; true",
    ] {
        let (mut x, i, check, _) = pending(command);
        let status = check_receipts::reconcile(&mut x.f.store).unwrap();
        assert_eq!(status["recovered"], json!([check]));
        assert_eq!(state(&x), "failed");
        let doc: String =
            x.f.store
                .connection
                .query_row("SELECT doc FROM check_runs WHERE id=?1", [check], |r| {
                    r.get(0)
                })
                .unwrap();
        let receipt: Value = serde_json::from_str(&doc).unwrap();
        if command.contains("base.txt") {
            assert!(receipt["checkout_error"].is_string());
        } else {
            assert_eq!(receipt["output"]["exit_code"], 7);
        }
        assert_eq!(
            check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
            json!([])
        );
        assert_eq!(x.advance(&i.id).state, "checks_failed");
    }
}

#[test]
fn check_receipt_recovery_is_leased_transactional_idempotent_and_fair() {
    let (mut x, _, check, _) = pending("true");
    let lease = crate::control::api::lock(&x.f.store.dir, "git-executor.lock").unwrap();
    assert!(check_receipts::reconcile(&mut x.f.store).is_err());
    drop(lease);
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_audit BEFORE INSERT ON journal WHEN NEW.kind='check_receipt_applied' BEGIN SELECT RAISE(ABORT,'lost check audit'); END;").unwrap();
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    assert_eq!(state(&x), "started");
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_audit;")
        .unwrap();
    let doc: String =
        x.f.store
            .connection
            .query_row("SELECT doc FROM check_runs WHERE id=?1", [&check], |r| {
                r.get(0)
            })
            .unwrap();
    for n in 0..16 {
        let mut initial: Value = serde_json::from_str(&doc).unwrap();
        let other = id();
        let name = format!("missing-{n}");
        initial["id"] = json!(other);
        initial["name"] = json!(name);
        x.f.store
            .connection
            .execute(
                "INSERT INTO check_runs VALUES (?1,?2,?3,'started',?4)",
                rusqlite::params![
                    other,
                    initial["integration"].as_str().unwrap(),
                    name,
                    initial.to_string()
                ],
            )
            .unwrap();
    }
    // The failed recovery's persisted cursor puts sixteen unavailable records
    // ahead of the finished one. The next bounded cycle must wrap around.
    let first = check_receipts::reconcile(&mut x.f.store).unwrap();
    assert_eq!(first["unresolved"].as_array().unwrap().len(), 16);
    assert_eq!(first["recovered"], json!([]));
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([check])
    );
    assert_eq!(state(&x), "passed");
    assert_eq!(
        check_receipts::reconcile(&mut x.f.store).unwrap()["recovered"],
        json!([])
    );
    let count: u64 =
        x.f.store
            .connection
            .query_row(
                "SELECT count(*) FROM journal WHERE kind='check_receipt_applied'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 1);
    x.f.store
        .connection
        .execute(
            "UPDATE check_runs SET doc=json_remove(doc,'$.receipt_scheme') WHERE state='started'",
            [],
        )
        .unwrap();
    let legacy = check_receipts::reconcile(&mut x.f.store).unwrap();
    assert_eq!(legacy["legacy_started"], 16);
    assert_eq!(legacy["remaining_started"], 16);
    assert_eq!(legacy["recovered"], json!([]));
}

#[tokio::test]
async fn check_receipt_controller_service_recovers_before_releasing_a_cancelled_repository() {
    use crate::control::{broker_policy::Endpoint, service, transport::Broker};
    let (mut x, i, check, _) = pending("printf x >> ../executions; true");
    cancel_with_exit(&mut x);
    assert_eq!(
        crate::control::recovery::cancelled(&mut x.f.store).unwrap()["released"],
        json!([])
    );
    let owner = x.f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = Broker::connect(&server.config("admin"), &owner, Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let config_path = x.f.dir.path().join("broker.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&server.config("controller")).unwrap(),
    )
    .unwrap();
    let status = service::controller(
        x.f.store.dir.clone(),
        config_path,
        x.config.clone(),
        true,
        None,
    )
    .await
    .unwrap();
    assert_eq!(status["check_receipts"]["recovered"], json!([check]));
    assert_eq!(status["recovery"]["released"].as_array().unwrap().len(), 1);
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "superseded"
    );
    let dir = x.f.store.dir.join("verification").join(&i.id);
    assert!(dir.join("checkout").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("executions")).unwrap(),
        "x"
    );
}
