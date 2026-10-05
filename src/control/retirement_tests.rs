use super::*;
use crate::control::retirement::{self, Request};

fn request(s: &Setup) -> Request {
    Request {
        operation: uuid::Uuid::new_v4().to_string(),
        attempt: s.attempt.id.clone(),
        expected_epoch: s.attempt.epoch,
        expected_head: s.plan.base_oid.clone(),
        processes_confirmed_stopped: true,
        retain_commits_remove_checkout: true,
        reason: "Isolated fixture is stopped; retain its commits".into(),
    }
}
fn retire(s: &Setup, r: &Request) -> anyhow::Result<Value> {
    retirement::retire_with(&s.node.dir, &s.config, r, &s.native)
}
fn count(s: &Setup) -> i64 {
    s.node
        .connection
        .query_row("SELECT count(*) FROM checkout_retirements", [], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn retirement_removes_only_confirmed_clean_checkout_and_retains_commits_and_replay() {
    let mut s = Setup::new();
    s.stop();
    s.native.removal.set(1);
    // A committed worker change remains reachable after removing its checkout.
    std::fs::write(s.plan.worktree.join("tracked"), "finished\n").unwrap();
    git(&s.plan.worktree, &["commit", "-am", "worker result"]);
    let mut r = request(&s);
    r.expected_head = git(&s.plan.worktree, &["rev-parse", "HEAD"]);
    let result = retire(&s, &r).unwrap();
    assert_eq!(result["state"], "removed");
    assert!(!s.plan.worktree.exists());
    assert_eq!(
        git(&s.plan.repo, &["rev-parse", &s.plan.branch]),
        r.expected_head
    );
    assert_eq!(
        git(
            &s.plan.repo,
            &["rev-parse", result["retained_ref"].as_str().unwrap()]
        ),
        r.expected_head
    );
    assert_eq!(s.native.remove_calls.get(), 1);
    assert_eq!(retire(&s, &r).unwrap(), result);
    assert_eq!(s.native.remove_calls.get(), 1);
    let mut changed = r.clone();
    changed.reason.push('!');
    assert!(retire(&s, &changed).is_err());
    changed = r.clone();
    changed.operation = uuid::Uuid::new_v4().to_string();
    assert!(retire(&s, &changed).is_err());
    assert!(!result.to_string().contains(&s.f.node));
}

#[test]
fn retirement_requires_operator_decision_positive_stop_and_exact_clean_ownership() {
    let mut s = Setup::new();
    s.native.removal.set(1);
    let mut r = request(&s);
    assert!(retire(&s, &r).is_err(), "controller stop absent");
    s.stop();
    r.processes_confirmed_stopped = false;
    assert!(retire(&s, &r).is_err());
    r.processes_confirmed_stopped = true;
    r.retain_commits_remove_checkout = false;
    assert!(retire(&s, &r).is_err());
    r.retain_commits_remove_checkout = true;
    r.expected_epoch += 1;
    assert!(retire(&s, &r).is_err());
    r.expected_epoch -= 1;
    r.expected_head = "f".repeat(40);
    assert!(retire(&s, &r).is_err());
    r.expected_head = s.plan.base_oid.clone();
    std::fs::write(s.plan.worktree.join("tracked"), "unsaved\n").unwrap();
    assert!(retire(&s, &r).is_err());
    git(&s.plan.worktree, &["checkout", "--", "tracked"]);
    std::fs::create_dir(s.plan.worktree.join("ignored")).unwrap();
    std::fs::write(s.plan.worktree.join("ignored/private"), "retained").unwrap();
    assert!(retire(&s, &r).is_err());
    std::fs::remove_dir_all(s.plan.worktree.join("ignored")).unwrap();
    s.native.terminal.set(2);
    assert!(retire(&s, &r).is_err());
    s.native.terminal.set(1);
    let lease = crate::control::api::lock(&s.node.dir, "execution.lock").unwrap();
    assert!(retire(&s, &r).is_err());
    drop(lease);
    assert_eq!(count(&s), 0);
    assert_eq!(s.native.remove_calls.get(), 0);
    assert!(s.plan.worktree.exists());
}

#[test]
fn retirement_lost_native_or_sql_reply_reconciles_once_without_repeating_removal() {
    for sql_loss in [false, true] {
        let mut s = Setup::new();
        s.stop();
        s.native.removal.set(2);
        let r = request(&s);
        if sql_loss {
            s.node.connection.execute_batch("CREATE TRIGGER lose_cleanup BEFORE UPDATE ON checkout_retirements WHEN NEW.state='removed' BEGIN SELECT RAISE(FAIL,'lost receipt'); END;").unwrap();
            assert!(retire(&s, &r).is_err());
            s.node
                .connection
                .execute_batch("DROP TRIGGER lose_cleanup;")
                .unwrap();
        }
        let receipt = retire(&s, &r).unwrap();
        assert_eq!(receipt["state"], "removed");
        assert_eq!(s.native.remove_calls.get(), 1);
        assert_eq!(retire(&s, &r).unwrap(), receipt);
        assert_eq!(count(&s), 1);
    }
}

#[test]
fn retirement_uncertain_or_partial_removal_never_repeats_native_effect() {
    for mode in [3, 4] {
        let mut s = Setup::new();
        s.stop();
        s.native.removal.set(mode);
        let r = request(&s);
        assert_eq!(retire(&s, &r).unwrap()["outcome"], "unresolved");
        assert_eq!(retire(&s, &r).unwrap()["outcome"], "unresolved");
        assert_eq!(s.native.remove_calls.get(), 1);
        // Only a separate operator action finishes this ambiguous fixture.
        if mode == 3 {
            git(
                &s.plan.repo,
                &["worktree", "remove", s.plan.worktree.to_str().unwrap()],
            );
        }
        s.native.resources_absent.set(true);
        assert_eq!(retire(&s, &r).unwrap()["state"], "removed");
        assert_eq!(s.native.remove_calls.get(), 1);
    }
}

#[test]
fn retirement_intent_failure_and_changed_retention_ref_preserve_checkout() {
    let mut s = Setup::new();
    s.stop();
    s.native.removal.set(1);
    let r = request(&s);
    s.node.connection.execute_batch("CREATE TRIGGER lose_intent BEFORE INSERT ON checkout_retirements BEGIN SELECT RAISE(FAIL,'lost intent'); END;").unwrap();
    assert!(retire(&s, &r).is_err());
    assert_eq!(s.native.remove_calls.get(), 0);
    assert_eq!(count(&s), 0);
    s.node
        .connection
        .execute_batch("DROP TRIGGER lose_intent;")
        .unwrap();
    let reference = format!("refs/herdr-supervisor/retained/{}", r.attempt);
    git(
        &s.plan.repo,
        &["commit", "--allow-empty", "-m", "unrelated"],
    );
    let other = git(&s.plan.repo, &["rev-parse", "HEAD"]);
    git(&s.plan.repo, &["update-ref", &reference, &other]);
    assert!(retire(&s, &r).is_err());
    assert_eq!(s.native.remove_calls.get(), 0);
    assert!(s.plan.worktree.exists());
    assert_eq!(git(&s.plan.repo, &["rev-parse", &reference]), other);
}

#[test]
fn retirement_schema_upgrade_keeps_jobs_and_durable_removal_survives_restart() {
    let mut s = Setup::new();
    s.stop();
    let before: String = s
        .node
        .connection
        .query_row("SELECT doc||checkpoint FROM jobs", [], |r| r.get(0))
        .unwrap();
    s.node
        .connection
        .execute_batch("DROP TABLE checkout_retirements; PRAGMA user_version=9;")
        .unwrap();
    drop(s.node);
    s.node = node_store(&s.f);
    let after: String = s
        .node
        .connection
        .query_row("SELECT doc||checkpoint FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    s.native.removal.set(1);
    let r = request(&s);
    let receipt = retire(&s, &r).unwrap();
    drop(s.node);
    s.node = node_store(&s.f);
    assert_eq!(retire(&s, &r).unwrap(), receipt);
    assert_eq!(s.native.remove_calls.get(), 1);
}

#[test]
fn retirement_operator_attestation_cannot_override_unresolved_or_foreign_native_exit() {
    let mut s = Setup::new();
    s.stop();
    s.native.removal.set(1);
    let r = request(&s);
    let valid = json!({"attempt":s.attempt.id,"epoch":s.attempt.epoch,"scope":"native_process"});
    for (state, receipt) in [
        ("running", valid.clone()),
        (
            "exited",
            json!({"attempt":"foreign","epoch":s.attempt.epoch,"scope":"native_process"}),
        ),
        (
            "exited",
            json!({"attempt":s.attempt.id,"epoch":s.attempt.epoch,"scope":"broker_receipt"}),
        ),
    ] {
        s.node
            .connection
            .execute(
                "INSERT OR REPLACE INTO runtime_processes VALUES (?1,?2,?3,?4)",
                rusqlite::params![s.attempt.id, s.attempt.epoch, state, receipt.to_string()],
            )
            .unwrap();
        assert!(retire(&s, &r).is_err());
        assert_eq!(count(&s), 0);
        assert_eq!(s.native.remove_calls.get(), 0);
    }
    s.node
        .connection
        .execute("UPDATE runtime_processes SET doc=?1", [valid.to_string()])
        .unwrap();
    assert_eq!(retire(&s, &r).unwrap()["state"], "removed");
}

#[test]
fn retirement_uncertain_reconciliation_fences_changed_authority_config_and_recreated_path() {
    let mut s = Setup::new();
    s.stop();
    s.native.removal.set(2);
    let r = request(&s);
    s.node.connection.execute_batch("CREATE TRIGGER lose_receipt BEFORE UPDATE ON checkout_retirements WHEN NEW.state='removed' BEGIN SELECT RAISE(FAIL,'receipt lost'); END;").unwrap();
    assert!(retire(&s, &r).is_err());
    s.node
        .connection
        .execute_batch("DROP TRIGGER lose_receipt")
        .unwrap();
    std::fs::create_dir(&s.plan.worktree).unwrap();
    std::fs::write(s.plan.worktree.join("unrelated"), "new owner").unwrap();
    let unresolved = retire(&s, &r).unwrap();
    assert_eq!(unresolved["outcome"], "unresolved");
    assert!(unresolved["resources_deleted"].is_null());
    assert_eq!(
        std::fs::read_to_string(s.plan.worktree.join("unrelated")).unwrap(),
        "new owner"
    );
    let original = s.config.repositories.clone();
    let repo = s.config.repositories.values().next().unwrap().clone();
    s.config.repositories = BTreeMap::from([("foreign-mapping".into(), repo)]);
    assert!(retire(&s, &r).is_err());
    s.config.repositories = original;
    s.node
        .connection
        .execute("UPDATE authority SET epoch=epoch+1", [])
        .unwrap();
    assert!(retire(&s, &r).is_err());
    assert_eq!(s.native.remove_calls.get(), 1);
}
