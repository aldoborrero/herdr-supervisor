use super::*;
use crate::control::{git_pipeline, verification};
use crate::runner::{Cmd, RealRunner, Runner};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn git(path: &Path, args: &[&str]) -> String {
    let out = RealRunner
        .run(
            &Cmd::new("git", Duration::from_secs(10))
                .cwd(path)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args.iter().copied()),
        )
        .unwrap();
    assert!(out.success(), "git {args:?}: {}", out.error_text());
    out.stdout.trim().into()
}
fn commit(path: &Path, file: &str, body: &str) -> String {
    std::fs::write(path.join(file), body).unwrap();
    git(path, &["add", "--", file]);
    git(path, &["commit", "-m", "fixture"]);
    git(path, &["rev-parse", "HEAD"])
}
struct GitFixture {
    f: Fixture,
    repo: PathBuf,
    author: PathBuf,
    attempt: Attempt,
    worker: String,
    result: String,
    config: git_pipeline::Config,
}
impl GitFixture {
    fn new(format: &str, check: &str, review: bool) -> Self {
        let mut f = Fixture::new();
        let repo = f.dir.path().join("repository");
        std::fs::create_dir(&repo).unwrap();
        git(
            &repo,
            &["init", "--quiet", &format!("--object-format={format}")],
        );
        let base = commit(&repo, "base.txt", "base\n");
        let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        run.base_oid = base.clone();
        run.integration_oid = base;
        run.policy.require_review = review;
        run.policy
            .checks
            .insert("test".into(), vec!["sh".into(), "-c".into(), check.into()]);
        f.store
            .connection
            .execute(
                "UPDATE runs SET doc=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&run).unwrap(), f.run],
            )
            .unwrap();
        let task = f.task();
        let (a, worker) = f.dispatch(&task);
        f.ready(&a, &worker);
        let author = f.dir.path().join("author");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "author",
                author.to_str().unwrap(),
                &run.base_oid,
            ],
        );
        let result = commit(&author, "feature.txt", "implemented\n");
        let config = git_pipeline::Config {
            repositories: BTreeMap::from([(
                "/repo".into(),
                git_pipeline::Repository {
                    path: repo.clone(),
                    node_remotes: BTreeMap::new(),
                },
            )]),
            check_timeout_secs: 5,
        };
        Self {
            f,
            repo,
            author,
            attempt: a,
            worker,
            result,
            config,
        }
    }
    fn submit(&mut self) {
        call(
            &mut self.f.store,
            &self.worker,
            Mutation::SubmitResult {
                attempt: self.attempt.id.clone(),
                epoch: 1,
                contract_hash: self.attempt.contract_hash.clone(),
                commit: Some(self.result.clone()),
                artifacts: BTreeMap::new(),
                summary: "Author claims every test passed".into(),
            },
        );
    }
    fn request(&mut self) -> Integration {
        let task: Task = document(&self.f.store.connection, "tasks", &self.attempt.task).unwrap();
        serde_json::from_value(call(
            &mut self.f.store,
            &self.f.supervisor,
            Mutation::RequestVerification {
                attempt: self.attempt.id.clone(),
                expected_revision: task.revision,
            },
        ))
        .unwrap()
    }
    fn advance(&mut self, id: &str) -> Integration {
        git_pipeline::advance(&mut self.f.store, &self.config, id, &RealRunner).unwrap()
    }
    fn reviewer(&mut self, i: &Integration) -> (Attempt, String) {
        let value = call(
            &mut self.f.store,
            &self.f.supervisor,
            Mutation::RequestReview {
                integration: i.id.clone(),
                expected_revision: i.revision,
                node: "local".into(),
                profile: "auto".into(),
            },
        );
        let a: Attempt = serde_json::from_value(value["attempt"].clone()).unwrap();
        let effect = self
            .f
            .store
            .pending(&self.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "execute" && e.payload["attempt"]["id"] == a.id)
            .unwrap();
        let token = text(&effect.payload, "credential");
        assert_eq!(
            effect.payload["review"]["candidate_oid"],
            i.candidate_oid.as_ref().unwrap().as_str()
        );
        let resource = Resource {
            repo_common_dir: "/repo/.git".into(),
            worktree: format!("/review/{}", a.id),
            branch: format!("hs/{}/{}", a.run, a.id),
            workspace: "review".into(),
            pane: format!("review:{}", a.id),
            terminal: id(),
            session: id(),
            runtime: "codex".into(),
            mailer_agent: format!("review-{}", a.id),
            mailer_epoch: 1,
        };
        call(
            &mut self.f.store,
            &self.f.node,
            Mutation::BindResource {
                attempt: a.id.clone(),
                epoch: 1,
                resource: resource.clone(),
            },
        );
        call(
            &mut self.f.store,
            &token,
            Mutation::WorkerReady {
                attempt: a.id.clone(),
                epoch: 1,
                session: resource.session,
            },
        );
        (a, token)
    }
    fn review(&mut self, i: &Integration, a: &Attempt, token: &str, approve: bool) -> Value {
        call(
            &mut self.f.store,
            token,
            Mutation::SubmitReview {
                attempt: a.id.clone(),
                epoch: 1,
                integration: i.id.clone(),
                candidate_oid: i.candidate_oid.clone().unwrap(),
                contract_hash: i.contract_hash.clone(),
                approve,
                criteria: vec![CriterionReview {
                    criterion: 0,
                    passed: approve,
                    evidence: format!("Inspected {}", i.candidate_oid.as_ref().unwrap()),
                }],
                findings: if approve {
                    vec![]
                } else {
                    vec!["Parser still fails the empty-input criterion".into()]
                },
            },
        )
    }
    fn queue_apply(&mut self, i: &Integration) {
        call(
            &mut self.f.store,
            &self.f.supervisor,
            Mutation::ApplyIntegration {
                integration: i.id.clone(),
                expected_revision: i.revision,
            },
        );
    }
    fn reference(&self) -> String {
        format!("refs/heads/hs/integration/{}", self.f.run)
    }
}

#[test]
fn combined_candidate_review_and_checks_gate_owned_branch_for_sha1_and_sha256() {
    for format in ["sha1", "sha256"] {
        let mut x = GitFixture::new(format, "test -f feature.txt && test -f accepted.txt", true);
        let accepted = commit(&x.repo, "accepted.txt", "already integrated\n");
        let mut run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
        run.integration_oid = accepted.clone();
        x.f.store
            .connection
            .execute(
                "UPDATE runs SET doc=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&run).unwrap(), x.f.run],
            )
            .unwrap();
        std::fs::write(x.repo.join("base.txt"), "uncommitted user edit\n").unwrap();
        x.submit();
        let requested = x.request();
        let i = x.advance(&requested.id);
        assert_eq!(i.state, "awaiting_review");
        assert_ne!(i.candidate_oid.as_ref().unwrap(), &x.result);
        let candidate = i.candidate_oid.clone().unwrap();
        assert_eq!(candidate.len(), if format == "sha256" { 64 } else { 40 });
        assert_eq!(
            git(&x.repo, &["show", &format!("{candidate}:feature.txt")]),
            "implemented"
        );
        assert_eq!(
            git(&x.repo, &["show", &format!("{candidate}:accepted.txt")]),
            "already integrated"
        );
        assert_eq!(git(&x.repo, &["rev-parse", "HEAD"]), accepted);
        assert_eq!(
            std::fs::read_to_string(x.repo.join("base.txt")).unwrap(),
            "uncommitted user edit\n"
        );
        assert!(
            !x.author.join("accepted.txt").exists(),
            "checks must inspect the combined candidate, not the author's branch"
        );
        assert!(
            x.f.store
                .apply(&request(
                    &x.f.supervisor,
                    Mutation::ApplyIntegration {
                        integration: i.id.clone(),
                        expected_revision: i.revision
                    }
                ))
                .is_err()
        );
        let (a, reviewer) = x.reviewer(&i);
        assert_eq!(a.role, "reviewer");
        assert_eq!(a.base_oid, candidate);
        let own_review = Mutation::SubmitReview {
            attempt: a.id.clone(),
            epoch: 1,
            integration: i.id.clone(),
            candidate_oid: candidate.clone(),
            contract_hash: i.contract_hash.clone(),
            approve: true,
            criteria: vec![CriterionReview {
                criterion: 0,
                passed: true,
                evidence: "claim".into(),
            }],
            findings: vec![],
        };
        assert!(
            x.f.store
                .apply(&request(&x.worker, own_review.clone()))
                .is_err()
        );
        let mut wrong = own_review.clone();
        if let Mutation::SubmitReview { candidate_oid, .. } = &mut wrong {
            *candidate_oid = x.result.clone();
        }
        assert!(x.f.store.apply(&request(&reviewer, wrong)).is_err());
        x.review(&i, &a, &reviewer, true);
        let verified = verification::integration(&x.f.store.connection, &i.id).unwrap();
        assert_eq!(verified.state, "verified");
        x.queue_apply(&verified);
        let applied = x.advance(&i.id);
        assert_eq!(applied.state, "applied");
        assert_eq!(git(&x.repo, &["rev-parse", &x.reference()]), candidate);
        assert_eq!(git(&x.repo, &["rev-parse", "HEAD"]), accepted);
        assert_eq!(
            std::fs::read_to_string(x.repo.join("base.txt")).unwrap(),
            "uncommitted user edit\n"
        );
        assert_eq!(
            document::<Task>(&x.f.store.connection, "tasks", &x.attempt.task)
                .unwrap()
                .state,
            "integrated"
        );
        assert_eq!(
            document::<Run>(&x.f.store.connection, "runs", &x.f.run)
                .unwrap()
                .integration_oid,
            candidate
        );
        let active: i64 =
            x.f.store
                .connection
                .query_row("SELECT count(*) FROM attempts WHERE active=1", [], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(active, 2, "approval does not prove either process stopped");
        let run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
        let complete = request(
            &x.f.supervisor,
            Mutation::CompleteRun {
                run: x.f.run.clone(),
                expected_revision: run.revision,
            },
        );
        assert!(x.f.store.apply(&complete).is_err());
        for attempt in [&x.attempt.id, &a.id] {
            call(
                &mut x.f.store,
                &x.f.node,
                Mutation::ObserveStopped {
                    attempt: attempt.clone(),
                    epoch: 1,
                },
            );
        }
        let response = x.f.store.apply(&complete).unwrap();
        assert_eq!(response["state"], "completed");
        assert_eq!(x.f.store.apply(&complete).unwrap(), response);
        let binding_active: i64 =
            x.f.store
                .connection
                .query_row(
                    "SELECT active FROM repository_bindings WHERE run=?1",
                    [&x.f.run],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(binding_active, 0);
    }
}

#[test]
fn failing_or_mutating_checks_cannot_be_replaced_by_an_author_claim() {
    for check in ["exit 7", "printf changed > base.txt"] {
        let mut x = GitFixture::new("sha1", check, false);
        x.submit();
        let i = x.request();
        let checked = x.advance(&i.id);
        assert_eq!(checked.state, "checks_failed");
        assert!(
            x.f.store
                .apply(&request(
                    &x.f.supervisor,
                    Mutation::ApplyIntegration {
                        integration: i.id,
                        expected_revision: checked.revision
                    }
                ))
                .is_err()
        );
        assert_eq!(
            git(&x.repo, &["rev-parse", &x.reference()]),
            x.attempt.base_oid
        );
        let count: i64 =
            x.f.store
                .connection
                .query_row(
                    "SELECT count(*) FROM check_runs WHERE state='failed'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn git_cas_after_sql_failure_is_reconciled_once_without_reapplying() {
    let mut x = GitFixture::new("sha1", "true", false);
    x.submit();
    let requested = x.request();
    let checked = x.advance(&requested.id);
    x.queue_apply(&checked);
    x.f.store.connection.execute_batch("CREATE TRIGGER fail_receipt BEFORE UPDATE ON integrations WHEN NEW.state='applied' BEGIN SELECT RAISE(ABORT,'injected SQL receipt failure'); END;").unwrap();
    assert!(git_pipeline::advance(&mut x.f.store, &x.config, &checked.id, &RealRunner).is_err());
    assert_eq!(
        git(&x.repo, &["rev-parse", &x.reference()]),
        checked.candidate_oid.as_ref().unwrap().as_str()
    );
    assert_eq!(
        verification::integration(&x.f.store.connection, &checked.id)
            .unwrap()
            .state,
        "applying"
    );
    assert_eq!(
        document::<Run>(&x.f.store.connection, "runs", &x.f.run)
            .unwrap()
            .integration_oid,
        checked.base_oid
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_receipt")
        .unwrap();
    let state = x.f.store.dir.clone();
    x.f.store = Store::open(&state).unwrap();
    assert_eq!(x.advance(&checked.id).state, "applied");
    assert_eq!(x.advance(&checked.id).state, "applied");
    let n: i64 =
        x.f.store
            .connection
            .query_row(
                "SELECT count(*) FROM journal WHERE kind='integration_applied'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn checked_out_integration_branch_and_changed_base_are_never_overwritten() {
    for checked_out in [true, false] {
        let mut x = GitFixture::new("sha1", "true", false);
        x.submit();
        let i = x.request();
        let verified = x.advance(&i.id);
        x.queue_apply(&verified);
        if checked_out {
            git(&x.repo, &["switch", &format!("hs/integration/{}", x.f.run)]);
        } else {
            git(&x.repo, &["update-ref", &x.reference(), &x.result]);
        }
        let before = git(&x.repo, &["rev-parse", &x.reference()]);
        assert!(git_pipeline::advance(&mut x.f.store, &x.config, &i.id, &RealRunner).is_err());
        assert_eq!(git(&x.repo, &["rev-parse", &x.reference()]), before);
        assert_ne!(
            document::<Task>(&x.f.store.connection, "tasks", &x.attempt.task)
                .unwrap()
                .state,
            "integrated"
        );
    }
}

#[test]
fn conflicts_are_explicit_and_do_not_change_either_checkout() {
    let mut x = GitFixture::new("sha1", "true", false);
    x.result = commit(&x.author, "base.txt", "author change\n");
    let current = commit(&x.repo, "base.txt", "other accepted change\n");
    let mut r: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    r.integration_oid = current.clone();
    x.f.store
        .connection
        .execute(
            "UPDATE runs SET doc=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&r).unwrap(), r.id],
        )
        .unwrap();
    x.submit();
    let i = x.request();
    let result = x.advance(&i.id);
    assert_eq!(result.state, "conflict");
    assert!(result.error.unwrap().contains("CONFLICT"));
    assert_eq!(git(&x.repo, &["rev-parse", "HEAD"]), current);
    assert_eq!(
        std::fs::read_to_string(x.author.join("base.txt")).unwrap(),
        "author change\n"
    );
    assert_eq!(
        std::fs::read_to_string(x.repo.join("base.txt")).unwrap(),
        "other accepted change\n"
    );
}

#[test]
fn started_check_is_not_rerun_after_unknown_execution_outcome() {
    let mut x = GitFixture::new("sha1", "true", false);
    x.submit();
    let i = x.request();
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_check BEFORE UPDATE ON check_runs BEGIN SELECT RAISE(ABORT,'lost receipt'); END;").unwrap();
    assert!(git_pipeline::advance(&mut x.f.store, &x.config, &i.id, &RealRunner).is_err());
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_check")
        .unwrap();
    assert_eq!(x.advance(&i.id).state, "checks_failed");
    let n: i64 =
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM check_runs", [], |r| r.get(0))
            .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn stopped_reviewer_can_be_replaced_and_rejected_task_retries_with_new_evidence() {
    let mut x = GitFixture::new("sha1", "true", true);
    x.submit();
    let requested = x.request();
    let i = x.advance(&requested.id);
    let (a, old_token) = x.reviewer(&i);
    let current = verification::integration(&x.f.store.connection, &i.id).unwrap();
    let review_request = Mutation::RequestReview {
        integration: i.id.clone(),
        expected_revision: current.revision,
        node: "local".into(),
        profile: "auto".into(),
    };
    assert!(
        x.f.store
            .apply(&request(&x.f.supervisor, review_request.clone()))
            .is_err()
    );
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::StopAttempt {
            attempt: a.id.clone(),
            expected_epoch: 1,
        },
    );
    assert!(
        x.f.store
            .apply(&request(&x.f.supervisor, review_request))
            .is_err(),
        "cancellation is not proof of exit"
    );
    call(
        &mut x.f.store,
        &x.f.node,
        Mutation::ObserveStopped {
            attempt: a.id.clone(),
            epoch: 2,
        },
    );
    let (b, new_token) = x.reviewer(&current);
    assert_ne!(old_token, new_token);
    x.review(&current, &b, &new_token, false);
    let task: Task = document(&x.f.store.connection, "tasks", &x.attempt.task).unwrap();
    let retry = Mutation::RetryTask {
        task: task.id.clone(),
        expected_revision: task.revision,
        contract: None,
    };
    assert!(
        x.f.store
            .apply(&request(&x.f.supervisor, retry.clone()))
            .is_err()
    );
    for attempt in [&x.attempt.id, &b.id] {
        call(
            &mut x.f.store,
            &x.f.node,
            Mutation::ObserveStopped {
                attempt: attempt.clone(),
                epoch: 1,
            },
        );
    }
    call(&mut x.f.store, &x.f.supervisor, retry);
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "superseded"
    );
    let (next, worker) = x.f.dispatch(&task.id);
    assert_eq!(
        next.number, 4,
        "reviewer attempts must not consume the author attempt budget"
    );
    x.f.ready(&next, &worker);
    call(
        &mut x.f.store,
        &worker,
        Mutation::SubmitResult {
            attempt: next.id,
            epoch: 1,
            contract_hash: next.contract_hash,
            commit: Some(x.result.clone()),
            artifacts: BTreeMap::new(),
            summary: "repair result".into(),
        },
    );
    let task: Task = document(&x.f.store.connection, "tasks", &task.id).unwrap();
    assert!(
        x.f.store
            .apply(&request(
                &x.f.supervisor,
                Mutation::RequestVerification {
                    attempt: x.attempt.id,
                    expected_revision: task.revision
                }
            ))
            .is_err(),
        "an old author result cannot be reused after retry"
    );
}

#[test]
fn remote_result_requires_configured_node_source_and_exact_ancestry() {
    let mut x = GitFixture::new("sha1", "test -f feature.txt", false);
    let remote = x.repo.clone();
    let controller = x.f.dir.path().join("controller-repository");
    let branch = git(&remote, &["symbolic-ref", "--short", "HEAD"]);
    git(
        &remote,
        &[
            "clone",
            "--no-local",
            "--single-branch",
            "--branch",
            &branch,
            remote.to_str().unwrap(),
            controller.to_str().unwrap(),
        ],
    );
    x.repo = controller.clone();
    x.config.repositories.get_mut("/repo").unwrap().path = controller;
    assert!(
        !RealRunner
            .run(
                &Cmd::new("git", Duration::from_secs(10))
                    .cwd(&x.repo)
                    .args(["cat-file", "-e", &x.result])
            )
            .unwrap()
            .success()
    );
    x.submit();
    let i = x.request();
    assert!(git_pipeline::advance(&mut x.f.store, &x.config, &i.id, &RealRunner).is_err());
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "preparing"
    );
    x.config
        .repositories
        .get_mut("/repo")
        .unwrap()
        .node_remotes
        .insert("local".into(), remote.to_string_lossy().into());
    let checked = x.advance(&i.id);
    assert_eq!(checked.state, "verified");
    assert_eq!(
        git(
            &x.repo,
            &[
                "rev-parse",
                &format!("refs/herdr-supervisor/results/{}", i.id)
            ]
        ),
        x.result
    );
    assert_eq!(git(&x.repo, &["rev-parse", "HEAD"]), x.attempt.base_oid);

    // A real commit on an unrelated root is still not a result for this assignment.
    let mut x = GitFixture::new("sha1", "true", false);
    git(&x.author, &["switch", "--orphan", "unrelated"]);
    x.result = commit(&x.author, "unrelated.txt", "unrelated\n");
    x.submit();
    let i = x.request();
    assert_eq!(x.advance(&i.id).state, "invalid_result");
    let count: i64 =
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM check_runs", [], |r| r.get(0))
            .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn migration_preserves_v2_authority_credentials_and_operation_replays() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
    let proposal_request = request(
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "Use the corpus".into(),
            source_attempt: Some(attempt.id.clone()),
            evidence: vec!["corpus test".into()],
        },
    );
    let proposal = f.store.apply(&proposal_request).unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    let wire = f.store.prepare_wire(&effect.id, &owner).unwrap();
    let before = f.store.snapshot(&f.supervisor, &f.run).unwrap();
    let journal: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap();
    // Restore the actual v2 schema, retaining the authoritative rows and signed outbox.
    f.store.connection.execute_batch("DROP TABLE verification_worktrees; DROP TABLE check_runs; DROP TABLE review_assignments; DROP TABLE repository_bindings; DROP INDEX active_integration; PRAGMA user_version=2;").unwrap();
    let state = f.store.dir.clone();
    f.store = Store::open(&state).unwrap();
    assert_eq!(f.store.snapshot(&f.supervisor, &f.run).unwrap(), before);
    assert_eq!(
        serde_json::to_value(f.store.prepare_wire(&effect.id, &owner).unwrap()).unwrap(),
        serde_json::to_value(wire).unwrap()
    );
    assert_eq!(f.store.apply(&proposal_request).unwrap(), proposal);
    assert_eq!(
        document::<Attempt>(&f.store.connection, "attempts", &attempt.id)
            .unwrap()
            .state,
        "reserved"
    );
    let after: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(after, journal);
    assert_eq!(f.store.owner().unwrap(), owner.did());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn continuous_controller_verifies_without_blocking_api_and_applies_owned_ref() {
    use crate::control::{api, service, transport::Broker};
    let mut x = GitFixture::new("sha1", "true", false);
    let started = x.f.dir.path().join("check-started");
    let release = x.f.dir.path().join("check-release");
    let mut run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    run.policy.checks.insert(
        "test".into(),
        vec![
            "sh".into(),
            "-c".into(),
            ": > \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.02; done; test -f feature.txt".into(),
            "check".into(),
            started.to_string_lossy().into(),
            release.to_string_lossy().into(),
        ],
    );
    save_run(&x.f.store.connection, &run).unwrap();
    x.config.check_timeout_secs = 10;
    x.submit();
    let i = x.request();
    let server = NatsServer::start(&x.f.store.owner().unwrap());
    let broker_config = server.config("admin");
    let broker = Broker::connect(&broker_config, &x.f.store.owner().unwrap())
        .await
        .unwrap();
    broker
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let config_path = x.f.dir.path().join("broker.json");
    std::fs::write(&config_path, serde_json::to_vec(&broker_config).unwrap()).unwrap();
    let service = tokio::spawn(service::controller(
        x.f.store.dir.clone(),
        config_path.clone(),
        x.config.clone(),
        true,
    ));
    tokio::time::timeout(Duration::from_secs(8), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let state = x.f.store.dir.clone();
    let request = api::ApiRequest::Snapshot {
        token: x.f.supervisor.clone(),
        run: x.f.run.clone(),
    };
    let snapshot = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || api::call(&state, &request)),
    )
    .await;
    // Release even if responsiveness regresses, so the failing test does not orphan a check.
    std::fs::write(&release, b"continue").unwrap();
    let snapshot = snapshot
        .expect("API was blocked by the check")
        .unwrap()
        .unwrap();
    assert!(snapshot.to_string().contains("checking"));
    let result = tokio::time::timeout(Duration::from_secs(10), service)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["verification"]["state"], "verified");
    assert!(!x.f.store.dir.join("control.sock").exists());
    let checked = verification::integration(&x.f.store.connection, &i.id).unwrap();
    x.queue_apply(&checked);
    let applied = service::controller(x.f.store.dir.clone(), config_path, x.config.clone(), true)
        .await
        .unwrap();
    assert_eq!(applied["verification"]["state"], "applied");
    assert_eq!(
        git(&x.repo, &["rev-parse", &x.reference()]),
        checked.candidate_oid.unwrap()
    );
}

#[tokio::test]
async fn assigned_reviewer_uses_signed_node_transport_and_receives_controller_receipt() {
    use crate::control::{
        transport::*,
        worker::{WorkerRequest, handle},
    };
    let mut x = GitFixture::new("sha1", "true", true);
    x.submit();
    let requested = x.request();
    let i = x.advance(&requested.id);
    let (reviewer, token) = x.reviewer(&i);
    let owner = x.f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let controller = Broker::connect(&server.config("admin"), &owner)
        .await
        .unwrap();
    controller
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let broker = Broker::connect(&server.config("node"), &owner)
        .await
        .unwrap();
    let mut node = node_store(&x.f);
    controller_once(&mut x.f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    let context = handle(
        &mut node,
        &x.f.node_key,
        WorkerRequest::Context {
            token: token.clone(),
            attempt: reviewer.id.clone(),
            epoch: 1,
        },
    )
    .unwrap();
    assert_eq!(
        context["review"]["candidate_oid"],
        i.candidate_oid.as_ref().unwrap().as_str()
    );
    let event = handle(
        &mut node,
        &x.f.node_key,
        WorkerRequest::Apply {
            token: token.clone(),
            operation_id: id(),
            command: Box::new(Mutation::SubmitReview {
                attempt: reviewer.id.clone(),
                epoch: 1,
                integration: i.id.clone(),
                candidate_oid: i.candidate_oid.clone().unwrap(),
                contract_hash: i.contract_hash.clone(),
                approve: true,
                criteria: vec![CriterionReview {
                    criterion: 0,
                    passed: true,
                    evidence: "Inspected the exact candidate and corpus".into(),
                }],
                findings: vec![],
            }),
        },
    )
    .unwrap();
    node_once(&mut node, &broker).await.unwrap();
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "awaiting_review",
        "a broker ACK cannot approve a review"
    );
    controller_once(&mut x.f.store, &controller).await.unwrap();
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "verified"
    );
    controller_once(&mut x.f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    let receipt = handle(
        &mut node,
        &x.f.node_key,
        WorkerRequest::Receipt {
            token,
            attempt: reviewer.id,
            epoch: 1,
            event: text(&event, "event"),
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "controller_applied");
    let rows: i64 =
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM reviews", [], |r| r.get(0))
            .unwrap();
    assert_eq!(rows, 1);
}
