use super::*;
use crate::control::verification_retirement::{self, Request};

fn ready(format: &str) -> (GitFixture, Integration) {
    let mut x = GitFixture::new(format, "true", false);
    x.submit();
    let request = x.request();
    let checked = x.advance(&request.id);
    x.queue_apply(&checked);
    let applied = x.advance(&request.id);
    assert_eq!(applied.state, "applied");
    call(
        &mut x.f.store,
        &x.f.node,
        Mutation::ObserveStopped {
            attempt: x.attempt.id.clone(),
            epoch: 1,
        },
    );
    let run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::CompleteRun {
            run: run.id,
            expected_revision: run.revision,
        },
    );
    (x, applied)
}
fn request(i: &Integration) -> Request {
    Request {
        operation: id(),
        integration: i.id.clone(),
        expected_revision: i.revision,
        expected_candidate: i.candidate_oid.clone().unwrap(),
        processes_confirmed_stopped: true,
        retain_commits_remove_checkout: true,
        reason: "All fixture checks and checkout users stopped".into(),
    }
}
fn retire(x: &mut GitFixture, r: &Request) -> anyhow::Result<Value> {
    verification_retirement::retire(&mut x.f.store, &x.config, &x.f.owner, r)
}
fn path(x: &GitFixture, i: &Integration) -> PathBuf {
    x.f.store
        .dir
        .join("verification")
        .join(&i.id)
        .join("checkout")
}

#[test]
fn verification_retirement_preserves_sha1_and_sha256_candidates_evidence_and_replay() {
    for format in ["sha1", "sha256"] {
        let (mut x, i) = ready(format);
        let r = request(&i);
        let checkout = path(&x, &i);
        let receipt = retire(&mut x, &r).unwrap();
        assert_eq!(receipt["state"], "removed");
        assert!(!checkout.exists());
        assert_eq!(
            git(
                &x.repo,
                &["rev-parse", receipt["retained_ref"].as_str().unwrap()]
            ),
            r.expected_candidate
        );
        assert_eq!(
            git(&x.repo, &["rev-parse", &x.reference()]),
            r.expected_candidate
        );
        assert_eq!(
            x.f.store
                .connection
                .query_row(
                    "SELECT count(*) FROM check_runs WHERE state='passed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert_eq!(retire(&mut x, &r).unwrap(), receipt);
        assert!(
            crate::control::recovery::blockers(&x.f.store.connection, &x.f.run)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            x.f.store
                .connection
                .query_row(
                    "SELECT count(*) FROM journal WHERE kind='verification_retirement_completed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(
            verification_retirement::retire(&mut x.f.store, &x.config, &x.f.supervisor, &r)
                .is_err()
        );
    }
}

#[test]
fn verification_retirement_requires_terminal_authority_and_clean_exact_checkout() {
    let (mut x, i) = ready("sha1");
    let mut r = request(&i);
    let p = path(&x, &i);
    r.processes_confirmed_stopped = false;
    assert!(retire(&mut x, &r).is_err());
    r.processes_confirmed_stopped = true;
    r.expected_revision += 1;
    assert!(retire(&mut x, &r).is_err());
    r.expected_revision -= 1;
    let mut unknown = i.clone();
    unknown.state = "unrecognized".into();
    verification::save(&x.f.store.connection, &unknown).unwrap();
    assert!(retire(&mut x, &r).is_err());
    verification::save(&x.f.store.connection, &i).unwrap();
    std::fs::write(p.join("unsaved"), "retain").unwrap();
    assert!(retire(&mut x, &r).is_err());
    std::fs::remove_file(p.join("unsaved")).unwrap();
    git(&p, &["update-index", "--assume-unchanged", "base.txt"]);
    assert!(retire(&mut x, &r).is_err());
    git(&p, &["update-index", "--no-assume-unchanged", "base.txt"]);
    x.f.store
        .connection
        .execute("UPDATE repository_bindings SET active=1", [])
        .unwrap();
    assert!(retire(&mut x, &r).is_err());
    x.f.store
        .connection
        .execute("UPDATE repository_bindings SET active=0", [])
        .unwrap();
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='started'", [])
        .unwrap();
    assert!(retire(&mut x, &r).is_err());
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='passed'", [])
        .unwrap();
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='unknown'", [])
        .unwrap();
    assert!(retire(&mut x, &r).is_err());
    x.f.store
        .connection
        .execute("UPDATE check_runs SET state='passed'", [])
        .unwrap();
    let lease = crate::control::api::lock(&x.f.store.dir, "git-executor.lock").unwrap();
    assert!(retire(&mut x, &r).is_err());
    drop(lease);
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM verification_retirements", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    assert!(p.exists());
    let reference = format!("refs/herdr-supervisor/retained-verification/{}", i.id);
    git(&x.repo, &["symbolic-ref", &reference, &x.reference()]);
    assert!(retire(&mut x, &r).is_err());
    assert!(p.exists());
}

#[test]
fn verification_retirement_audit_failure_rolls_back_intent_and_recovers_lost_final_receipt() {
    let (mut x, i) = ready("sha1");
    let r = request(&i);
    let p = path(&x, &i);
    x.f.store.connection.execute_batch("CREATE TRIGGER fail_retire BEFORE INSERT ON journal WHEN NEW.kind='verification_retirement_prepared' BEGIN SELECT RAISE(FAIL,'audit failed'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM verification_retirements", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    assert!(p.exists());
    x.f.store.connection.execute_batch("DROP TRIGGER fail_retire; CREATE TRIGGER fail_retire BEFORE INSERT ON journal WHEN NEW.kind='verification_retirement_completed' BEGIN SELECT RAISE(FAIL,'audit failed'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    assert!(!p.exists());
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT state FROM verification_worktrees", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "ready"
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_retire")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(retire(&mut x, &r).unwrap()["state"], "removed");
    let mut changed = request(&i);
    changed.operation = r.operation.clone();
    changed.reason.push('!');
    assert!(retire(&mut x, &changed).is_err());
}

#[test]
fn verification_retirement_migrates_schema_and_never_deletes_recreated_path() {
    let (mut x, i) = ready("sha1");
    x.f.store
        .connection
        .execute_batch("DROP TABLE task_questions; DROP TABLE artifact_retirements; DROP TABLE coordinator_retirements; DROP TABLE verification_retirements; PRAGMA user_version=11")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    let r = request(&i);
    let p = path(&x, &i);
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_receipt BEFORE INSERT ON journal WHEN NEW.kind='verification_retirement_completed' BEGIN SELECT RAISE(FAIL,'lost receipt'); END;").unwrap();
    assert!(retire(&mut x, &r).is_err());
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_receipt")
        .unwrap();
    std::fs::create_dir(&p).unwrap();
    std::fs::write(p.join("new-data"), "keep").unwrap();
    let uncertain = retire(&mut x, &r).unwrap();
    assert_eq!(uncertain["outcome"], "unresolved");
    assert!(uncertain["resources_deleted"].is_null());
    assert_eq!(std::fs::read_to_string(p.join("new-data")).unwrap(), "keep");
}

#[test]
fn verification_retirement_after_cancelled_release_does_not_block_repository_reuse() {
    let mut x = GitFixture::new("sha1", "true", false);
    x.submit();
    let initial = x.request();
    let checked = x.advance(&initial.id);
    assert!(
        retire(&mut x, &request(&checked)).is_err(),
        "active run cannot retire"
    );
    cancel_with_exit(&mut x);
    assert!(
        retire(&mut x, &request(&checked)).is_err(),
        "unreleased binding cannot retire"
    );
    crate::control::recovery::cancelled(&mut x.f.store).unwrap();
    let terminal = verification::integration(&x.f.store.connection, &initial.id).unwrap();
    assert_eq!(terminal.state, "superseded");
    assert_eq!(
        retire(&mut x, &request(&terminal)).unwrap()["state"],
        "removed"
    );
    let next = replacement_run(&x);
    assert!(x.f.store.apply(&next).is_ok());
}

#[test]
fn verification_retirement_never_repeats_an_uncertain_git_removal() {
    struct NoRemoval(std::cell::Cell<u32>);
    impl crate::runner::Runner for NoRemoval {
        fn run(&self, cmd: &crate::runner::Cmd) -> anyhow::Result<crate::runner::Output> {
            if cmd.args.windows(2).any(|a| a == ["worktree", "remove"]) {
                self.0.set(self.0.get() + 1);
                return Ok(crate::runner::Output {
                    timed_out: true,
                    ..Default::default()
                });
            }
            RealRunner.run(cmd)
        }
        fn socket_request(
            &self,
            _: &std::path::Path,
            _: &str,
            _: std::time::Duration,
        ) -> anyhow::Result<String> {
            panic!("no sockets")
        }
        fn run_foreground(
            &self,
            _: &crate::runner::Cmd,
            _: &mut dyn FnMut() -> bool,
        ) -> anyhow::Result<Option<i32>> {
            panic!("no models")
        }
    }
    let (mut x, i) = ready("sha1");
    let r = request(&i);
    let runner = NoRemoval(std::cell::Cell::new(0));
    for _ in 0..2 {
        let result = verification_retirement::retire_with(
            &mut x.f.store,
            &x.config,
            &x.f.owner,
            &r,
            &runner,
        )
        .unwrap();
        assert_eq!(result["outcome"], "unresolved");
        assert!(result["resources_deleted"].is_null());
    }
    assert_eq!(runner.0.get(), 1);
    let checkout = path(&x, &i);
    git(&x.repo, &["worktree", "remove", checkout.to_str().unwrap()]);
    let result =
        verification_retirement::retire_with(&mut x.f.store, &x.config, &x.f.owner, &r, &runner)
            .unwrap();
    assert_eq!(result["state"], "removed");
    assert_eq!(runner.0.get(), 1);
}
