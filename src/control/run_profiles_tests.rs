use super::*;
use crate::control::run_profiles;

fn set(f: &mut Fixture, name: &str, profile: Option<Profile>) -> Value {
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.owner,
        Mutation::SetProfile {
            run: f.run.clone(),
            expected_revision: run.revision,
            name: name.into(),
            profile,
        },
    )
}
fn profile(runtime: &str, limit: u32) -> Profile {
    Profile {
        runtime: runtime.into(),
        argv: vec!["--model".into(), "owner-model".into()],
        limit,
    }
}

#[test]
fn run_profiles_are_owner_scoped_revisioned_and_cannot_override_permissions_or_remove_last_profile()
{
    assert!(serde_json::from_value::<Mutation>(json!({"method":"set_profile","params":{"run":"run","expected_revision":1,"name":"auto"}})).is_err(), "Removal requires explicit profile:null");
    let mut f = Fixture::new();
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let req = request(
        &f.owner,
        Mutation::SetProfile {
            run: f.run.clone(),
            expected_revision: run.revision,
            name: "review".into(),
            profile: Some(profile("pi", 1)),
        },
    );
    let mut denied = Request {
        token: req.token.clone(),
        operation_id: req.operation_id.clone(),
        command: req.command.clone(),
    };
    denied.token = f.supervisor.clone();
    assert!(
        f.store
            .apply(&denied)
            .unwrap_err()
            .to_string()
            .contains("owner capability")
    );
    let first = f.store.apply_in_run(&f.run, &req).unwrap();
    assert_eq!(first["revision"], run.revision + 1);
    assert_eq!(f.store.apply_in_run(&f.run, &req).unwrap(), first);
    let mut stale = Request {
        token: req.token.clone(),
        operation_id: req.operation_id.clone(),
        command: req.command.clone(),
    };
    stale.operation_id = id();
    assert!(
        f.store
            .apply(&stale)
            .unwrap_err()
            .to_string()
            .contains("stale revision")
    );
    let mut unsafe_profile = profile("codex", 1);
    unsafe_profile.argv = vec!["--dangerously-bypass-approvals-and-sandbox".into()];
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    assert!(
        f.store
            .apply(&request(
                &f.owner,
                Mutation::SetProfile {
                    run: f.run.clone(),
                    expected_revision: r.revision,
                    name: "unsafe".into(),
                    profile: Some(unsafe_profile)
                }
            ))
            .is_err()
    );
    let other = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "other".into(),
            repo: "/other".into(),
            base_oid: BASE.into(),
            objective: "Other".into(),
            policy: policy(),
        },
    );
    assert!(f.store.apply_in_run(&text(&other, "id"), &req).is_err());
    set(&mut f, "auto", None);
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    assert!(
        f.store
            .apply(&request(
                &f.owner,
                Mutation::SetProfile {
                    run: f.run.clone(),
                    expected_revision: r.revision,
                    name: "review".into(),
                    profile: None
                }
            ))
            .is_err()
    );
    let current: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    assert_eq!(current.revision, r.revision);
    assert!(current.policy.profiles.contains_key("review"));
}

#[test]
fn run_profiles_edit_and_removal_preserve_reserved_assignments_and_future_capacity_limits() {
    let mut f = Fixture::new();
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    run.policy.max_active = 3;
    save_run(&f.store.connection, &run).unwrap();
    let a = f.task();
    let b = f.task();
    let (old, old_token) = f.dispatch(&a);
    let before: String = f
        .store
        .connection
        .query_row("SELECT payload FROM outbox WHERE kind='execute'", [], |r| {
            r.get(0)
        })
        .unwrap();
    set(&mut f, "auto", Some(profile("claude", 1)));
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::Dispatch {
                    task: b.clone(),
                    expected_revision: 1,
                    node: "local".into(),
                    profile: "auto".into()
                }
            ))
            .is_err()
    );
    assert_eq!(
        run_profiles::assigned(&f.store.connection, &old.id).unwrap(),
        policy().profiles["auto"]
    );
    f.ready(&old, &old_token); // Old Codex binds even though future auto is Claude.
    set(&mut f, "auto", Some(profile("claude", 2)));
    let (new, _) = f.dispatch(&b);
    assert_eq!(
        run_profiles::assigned(&f.store.connection, &new.id).unwrap(),
        profile("claude", 2)
    );
    let unchanged: String = f.store.connection.query_row("SELECT payload FROM outbox WHERE kind='execute' AND json_extract(payload,'$.attempt.id')=?1", [&old.id], |r|r.get(0)).unwrap();
    assert_eq!(before, unchanged);
    set(&mut f, "replacement", Some(profile("pi", 2)));
    set(&mut f, "auto", None);
    let resource = Resource {
        repo_common_dir: "/repo/.git".into(),
        worktree: format!("/worktrees/{}", new.id),
        branch: format!("hs/{}/{}", new.run, new.id),
        workspace: "w2".into(),
        pane: id(),
        terminal: id(),
        session: id(),
        runtime: "claude".into(),
        mailer_agent: id(),
        mailer_epoch: 1,
    };
    let mut wrong = resource.clone();
    wrong.runtime = "pi".into();
    assert!(
        f.store
            .apply(&request(
                &f.node,
                Mutation::BindResource {
                    attempt: new.id.clone(),
                    epoch: 1,
                    resource: wrong
                }
            ))
            .is_err()
    );
    assert!(
        f.store
            .apply(&request(
                &f.node,
                Mutation::BindResource {
                    attempt: new.id.clone(),
                    epoch: 1,
                    resource
                }
            ))
            .is_ok()
    );
    let snapshot = f.store.snapshot(&f.owner, &f.run).unwrap();
    assert_eq!(snapshot["active_attempts"], 2);
    assert!(
        snapshot["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == old.id && a["profile_snapshot"]["runtime"] == "codex")
    );
}

#[test]
fn run_profiles_snapshot_and_profile_update_roll_back_with_failed_outbox_or_journal() {
    let mut f = Fixture::new();
    let task = f.task();
    f.store.connection.execute_batch("CREATE TRIGGER fail_dispatch BEFORE INSERT ON outbox WHEN NEW.kind='execute' BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::Dispatch {
                    task,
                    expected_revision: 1,
                    node: "local".into(),
                    profile: "auto".into()
                }
            ))
            .is_err()
    );
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT COUNT(*) FROM attempt_profiles", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    f.store.connection.execute_batch("DROP TRIGGER fail_dispatch; CREATE TRIGGER fail_profile BEFORE INSERT ON journal WHEN NEW.kind='set_profile' BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    let req = request(
        &f.owner,
        Mutation::SetProfile {
            run: f.run.clone(),
            expected_revision: run.revision,
            name: "auto".into(),
            profile: Some(profile("pi", 1)),
        },
    );
    assert!(f.store.apply(&req).is_err());
    let after: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    assert_eq!(after.policy, run.policy);
    assert_eq!(after.revision, run.revision);
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_profile")
        .unwrap();
    assert!(f.store.apply(&req).is_ok());
}

#[test]
fn run_profiles_v7_migration_preserves_signed_bytes_and_original_profile_after_policy_change() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
    let effect = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "execute")
        .unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let signed = f.store.prepare_wire(&effect.id, &owner).unwrap();
    let before = f
        .store
        .connection
        .query_row("SELECT wire FROM outbox WHERE id=?1", [&effect.id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    // A newer current policy cannot be used as evidence of an old assignment.
    set(&mut f, "auto", Some(profile("pi", 1)));
    f.store
        .connection
        .execute_batch(
            "DROP TABLE attempt_profiles; DROP TABLE integration_policies; DROP TABLE event_recoveries; DROP TABLE registration_recoveries; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision; PRAGMA user_version=7;",
        )
        .unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(
        run_profiles::assigned(&f.store.connection, &attempt.id).unwrap(),
        policy().profiles["auto"]
    );
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT wire FROM outbox WHERE id=?1", [&effect.id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        before
    );
    assert_eq!(
        f.store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        12
    );
    assert_eq!(signed.record.payload["body"]["profile"]["runtime"], "codex");
}

#[test]
fn run_profiles_migration_rejects_conflicting_frozen_intent_without_partial_upgrade() {
    let mut f = Fixture::new();
    let task = f.task();
    f.dispatch(&task);
    let effect = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "execute")
        .unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    f.store.prepare_wire(&effect.id, &owner).unwrap();
    f.store.connection.execute_batch("DROP TABLE attempt_profiles; DROP TABLE integration_policies; DROP TABLE event_recoveries; DROP TABLE registration_recoveries; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision; PRAGMA user_version=7; UPDATE outbox SET payload=json_set(payload,'$.profile.runtime','pi') WHERE kind='execute';").unwrap();
    assert!(Store::open(&f.store.dir).is_err());
    assert_eq!(
        f.store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        7
    );
    assert_eq!(f.store.connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('attempt_profiles','integration_policies')",[],|r|r.get::<_,u64>(0)).unwrap(),0);
}

#[test]
fn run_profiles_missing_legacy_order_stays_unknown_instead_of_borrowing_current_policy() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
    f.store.connection.execute_batch("DROP TABLE attempt_profiles; DROP TABLE integration_policies; DELETE FROM outbox WHERE kind='execute'; DROP TABLE event_recoveries; DROP TABLE registration_recoveries; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision; PRAGMA user_version=7;").unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    assert!(run_profiles::assigned(&f.store.connection, &attempt.id).is_err());
    let snapshot = f.store.snapshot(&f.owner, &f.run).unwrap();
    assert_eq!(snapshot["active_attempts"], 1);
    assert!(snapshot["attempts"][0]["profile_snapshot"].is_null());
}
