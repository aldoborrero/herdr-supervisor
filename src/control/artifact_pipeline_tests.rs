use super::artifacts_tests::{chunks, fixture, put, result};
use super::*;
use crate::control::{
    artifact_inputs, artifact_review, artifacts::CHUNK_BYTES, git_pipeline, verification,
};
use crate::runner::RealRunner;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Artifacts {
    f: Fixture,
    author: Attempt,
    integration: Integration,
}

impl Artifacts {
    fn new(check: &str, review: bool) -> Self {
        Self::with_bytes(check, review, b"verified evidence\n")
    }
    fn with_bytes(check: &str, review: bool, bytes: &[u8]) -> Self {
        let (mut f, author, token, _) = fixture();
        // Configure the fixture's owner policy before sealing the verifier intent.
        let mut r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        r.policy.require_review = review;
        r.policy
            .checks
            .insert("test".into(), vec!["sh".into(), "-c".into(), check.into()]);
        save_run(&f.store.connection, &r).unwrap();
        for chunk in chunks("report.txt", bytes) {
            f.store.apply(&put(&token, &author, chunk)).unwrap();
        }
        f.store
            .apply(&result(
                &token,
                &author,
                BTreeMap::from([("report.txt".into(), digest(bytes))]),
            ))
            .unwrap();
        let task: Task = document(&f.store.connection, "tasks", &author.task).unwrap();
        let integration = serde_json::from_value(call(
            &mut f.store,
            &f.supervisor,
            Mutation::RequestVerification {
                attempt: author.id.clone(),
                expected_revision: task.revision,
            },
        ))
        .unwrap();
        Self {
            f,
            author,
            integration,
        }
    }
    fn config() -> git_pipeline::Config {
        // /repo does not exist and there is no Git repository binding/configuration.
        git_pipeline::Config {
            repositories: BTreeMap::new(),
            check_timeout_secs: 5,
        }
    }
    fn advance(&mut self) -> anyhow::Result<Integration> {
        git_pipeline::advance(
            &mut self.f.store,
            &Self::config(),
            &self.integration.id,
            &RealRunner,
        )
    }
    fn inputs(&self) -> PathBuf {
        self.f
            .store
            .dir
            .join("verification")
            .join(&self.integration.id)
            .join("inputs")
    }
    fn apply(&mut self) {
        let i = verification::integration(&self.f.store.connection, &self.integration.id).unwrap();
        call(
            &mut self.f.store,
            &self.f.supervisor,
            Mutation::ApplyIntegration {
                integration: i.id,
                expected_revision: i.revision,
            },
        );
    }
}

#[test]
fn artifact_pipeline_acceptance_completes_task_without_git_or_releasing_process_capacity() {
    let mut x = Artifacts::new("test \"$(cat report.txt)\" = 'verified evidence'", false);
    let verified = x.advance().unwrap();
    assert_eq!(verified.state, "verified");
    assert!(verified.candidate_oid.is_none());
    assert!(verified.tree_oid.is_none());
    assert!(verified.result_oid.is_empty());
    let receipt: String =
        x.f.store
            .connection
            .query_row("SELECT doc FROM check_runs", [], |r| r.get(0))
            .unwrap();
    let receipt: Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(
        receipt["candidate"],
        verified.artifact.as_ref().unwrap().reference()
    );
    assert_eq!(receipt["cwd"], x.inputs().to_str().unwrap());
    x.apply();
    assert_eq!(x.advance().unwrap().state, "applied");
    assert_eq!(
        document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
            .unwrap()
            .state,
        "completed"
    );
    assert_eq!(
        document::<Run>(&x.f.store.connection, "runs", &x.f.run)
            .unwrap()
            .integration_oid,
        BASE
    );
    assert_eq!(
        x.f.store
            .connection
            .query_row("SELECT count(*) FROM repository_bindings", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        x.f.store.snapshot(&x.f.owner, &x.f.run).unwrap()["active_attempts"],
        1
    );
    assert_eq!(x.advance().unwrap().state, "applied");
    let r: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    assert!(
        x.f.store
            .apply(&request(
                &x.f.supervisor,
                Mutation::CompleteRun {
                    run: r.id,
                    expected_revision: r.revision,
                }
            ))
            .is_err(),
        "accepting files is not a process exit"
    );
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::StopAttempt {
            attempt: x.author.id.clone(),
            expected_epoch: x.author.epoch,
        },
    );
    let retired: Attempt = document(&x.f.store.connection, "attempts", &x.author.id).unwrap();
    call(
        &mut x.f.store,
        &x.f.node,
        Mutation::ObserveStopped {
            attempt: retired.id,
            epoch: retired.epoch,
        },
    );
    let r: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    let finished = call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::CompleteRun {
            run: r.id,
            expected_revision: r.revision,
        },
    );
    assert_eq!(finished["state"], "completed");
}

#[test]
fn artifact_pipeline_successful_check_cannot_modify_its_inputs() {
    let mut x = Artifacts::new("chmod u+w report.txt && printf forged > report.txt", false);
    assert_eq!(x.advance().unwrap().state, "checks_failed");
    let i = verification::integration(&x.f.store.connection, &x.integration.id).unwrap();
    assert!(
        x.f.store
            .apply(&request(
                &x.f.supervisor,
                Mutation::ApplyIntegration {
                    integration: i.id,
                    expected_revision: i.revision
                }
            ))
            .is_err()
    );
    assert_ne!(
        document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
            .unwrap()
            .state,
        "completed"
    );
}

#[test]
fn artifact_pipeline_materialization_recovers_only_owned_incomplete_inputs() {
    let x = Artifacts::new("true", false);
    let path = x.inputs();
    private_dir(&x.f.store.dir.join("verification")).unwrap();
    private_dir(path.parent().unwrap()).unwrap();
    private_dir(&path).unwrap();
    assert!(artifact_inputs::controller(&x.f.store, &x.integration).is_err());
    std::fs::remove_dir(&path).unwrap();
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_ready BEFORE UPDATE ON verification_worktrees WHEN NEW.state='ready' BEGIN SELECT RAISE(ABORT,'lost ready receipt'); END;").unwrap();
    assert!(artifact_inputs::controller(&x.f.store, &x.integration).is_err());
    assert_eq!(
        std::fs::read(path.join("report.txt")).unwrap(),
        b"verified evidence\n"
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_ready;")
        .unwrap();
    assert_eq!(
        artifact_inputs::controller(&x.f.store, &x.integration).unwrap(),
        path
    );
    std::fs::set_permissions(
        path.join("report.txt"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(path.join("report.txt"), "forged").unwrap();
    assert!(artifact_inputs::controller(&x.f.store, &x.integration).is_err());
    assert_eq!(std::fs::read(path.join("report.txt")).unwrap(), b"forged");
    std::fs::remove_file(path.join("report.txt")).unwrap();
    let target = x.f.dir.path().join("external");
    std::fs::write(&target, b"verified evidence\n").unwrap();
    std::os::unix::fs::symlink(&target, path.join("report.txt")).unwrap();
    assert!(artifact_inputs::controller(&x.f.store, &x.integration).is_err());
}

#[test]
fn artifact_pipeline_lost_check_receipt_does_not_repeat_external_command() {
    let mut x = Artifacts::new("printf x >> execution-count", false);
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_check BEFORE UPDATE ON check_runs BEGIN SELECT RAISE(ABORT,'lost check receipt'); END;").unwrap();
    assert!(x.advance().is_err());
    assert_eq!(
        std::fs::read(x.inputs().join("execution-count")).unwrap(),
        b"x"
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_check;")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(x.advance().unwrap().state, "checks_failed");
    assert_eq!(
        std::fs::read(x.inputs().join("execution-count")).unwrap(),
        b"x"
    );
}

#[test]
fn artifact_pipeline_acceptance_rolls_back_and_cancelled_retry_cannot_complete() {
    let mut x = Artifacts::new("true", false);
    x.advance().unwrap();
    x.apply();
    x.f.store.connection.execute_batch("CREATE TRIGGER lose_acceptance BEFORE INSERT ON journal WHEN NEW.kind='integration_applied' BEGIN SELECT RAISE(ABORT,'lost acceptance'); END;").unwrap();
    assert!(x.advance().is_err());
    assert_eq!(
        verification::integration(&x.f.store.connection, &x.integration.id)
            .unwrap()
            .state,
        "applying"
    );
    assert_ne!(
        document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
            .unwrap()
            .state,
        "completed"
    );
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER lose_acceptance;")
        .unwrap();
    let r: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::Cancel {
            run: r.id,
            expected_revision: r.revision,
        },
    );
    assert_eq!(x.advance().unwrap().state, "superseded");
    let stopped: Attempt = document(&x.f.store.connection, "attempts", &x.author.id).unwrap();
    call(
        &mut x.f.store,
        &x.f.node,
        Mutation::ObserveStopped {
            attempt: x.author.id.clone(),
            epoch: stopped.epoch,
        },
    );
    assert_eq!(
        document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
            .unwrap()
            .state,
        "cancelled"
    );
    assert_eq!(
        document::<Run>(&x.f.store.connection, "runs", &x.f.run)
            .unwrap()
            .integration_oid,
        BASE
    );
}

#[test]
fn artifact_pipeline_acceptance_revalidates_source_and_materialized_bytes() {
    for source in [true, false] {
        let mut x = Artifacts::new("true", false);
        x.advance().unwrap();
        x.apply();
        if source {
            x.f.store
                .connection
                .execute(
                    "UPDATE artifact_chunks SET data=?1",
                    [b"forged evidence!!\n".as_slice()],
                )
                .unwrap();
        } else {
            let path = x.inputs().join("report.txt");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(&path, "forged evidence!!\n").unwrap();
        }
        assert!(x.advance().is_err());
        assert_ne!(
            document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
                .unwrap()
                .state,
            "completed"
        );
    }
}

#[tokio::test]
async fn artifact_pipeline_cross_node_review_fetches_exact_content_over_restricted_nats() {
    use crate::control::{
        broker_policy::Endpoint,
        node::{ExecuteOrder, NodeStore},
        transport::{Broker, controller_once, node_once},
        worker::{self, WorkerRequest},
    };
    let bytes: Vec<u8> = (0..CHUNK_BYTES * 17 + 9).map(|n| (n % 251) as u8).collect();
    let mut x = Artifacts::with_bytes("true", true, &bytes);
    let run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    let bound = call(
        &mut x.f.store,
        &x.f.owner,
        Mutation::BindCoordinator {
            run: run.id,
            expected_revision: run.revision,
            endpoint: crate::control::coordinator::Endpoint {
                agent: "supervisor".into(),
                mailer_session: id(),
                mailer_epoch: 1,
                runtime: "codex".into(),
                native_session: id(),
            },
        },
    );
    x.f.supervisor = text(&bound, "credential");
    let i = x.advance().unwrap();
    assert_eq!(i.state, "awaiting_review");
    let owner = Identity::load(&x.f.store.dir.join("owner.key")).unwrap();
    let key = Identity::generate().unwrap();
    let node_token = text(
        &call(
            &mut x.f.store,
            &x.f.owner,
            Mutation::EnrollNode {
                id: "other".into(),
                key: key.did(),
                max_active: 1,
            },
        ),
        "credential",
    );
    let assigned = call(
        &mut x.f.store,
        &x.f.supervisor,
        Mutation::RequestReview {
            integration: i.id.clone(),
            expected_revision: i.revision,
            node: "other".into(),
            profile: "auto".into(),
        },
    );
    let reviewer: Attempt = serde_json::from_value(assigned["attempt"].clone()).unwrap();
    assert_eq!(reviewer.base_oid, BASE);
    assert_ne!(reviewer.node, x.author.node);
    let grant = owner
        .delegation("other", &key.did(), 1, now() + 3600)
        .unwrap();
    let dir = x.f.dir.path().join("other");
    let mut node = NodeStore::open(&dir, "other", &owner.did(), grant.clone(), &key).unwrap();
    let server = NatsServer::start(&owner.did());
    let admin = Broker::connect(&server.config("admin"), &owner.did(), Endpoint::Provisioner)
        .await
        .unwrap();
    admin
        .provision(&["controller".into(), "local".into(), "other".into()])
        .await
        .unwrap();
    let controller = Broker::connect(
        &server.config("controller"),
        &owner.did(),
        Endpoint::Controller,
    )
    .await
    .unwrap();
    let broker = Broker::connect(
        &server.config("other"),
        &owner.did(),
        Endpoint::Node("other"),
    )
    .await
    .unwrap();
    controller_once(&mut x.f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    let doc: String = node
        .connection
        .query_row("SELECT doc FROM jobs WHERE id=?1", [&reviewer.id], |r| {
            r.get(0)
        })
        .unwrap();
    let order: ExecuteOrder = serde_json::from_str(&doc).unwrap();
    let review = order.review.as_ref().unwrap();
    assert!(review.candidate_oid.is_empty());
    assert_eq!(review.artifact, i.artifact);
    assert_eq!(order.attempt.base_oid, BASE);
    let worktree = x.f.dir.path().join("review-worktree");
    private_dir(&worktree).unwrap();
    let resource = Resource {
        repo_common_dir: "/other/repo/.git".into(),
        worktree: worktree.to_string_lossy().into(),
        branch: format!("hs/{}/{}", reviewer.run, reviewer.id),
        workspace: "review-space".into(),
        pane: "review-pane".into(),
        terminal: "review-terminal".into(),
        session: id(),
        runtime: "codex".into(),
        mailer_agent: "independent-reviewer".into(),
        mailer_epoch: 1,
    };
    node.enqueue(
        &request(
            &node_token,
            Mutation::BindResource {
                attempt: reviewer.id.clone(),
                epoch: reviewer.epoch,
                resource: resource.clone(),
            },
        ),
        &key,
    )
    .unwrap();
    node_once(&mut node, &broker).await.unwrap();
    controller_once(&mut x.f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    assert_eq!(
        document::<Attempt>(&x.f.store.connection, "attempts", &reviewer.id)
            .unwrap()
            .state,
        "awaiting_ready"
    );
    let review_command = Mutation::SubmitArtifactReview {
        attempt: reviewer.id.clone(),
        epoch: reviewer.epoch,
        integration: i.id.clone(),
        manifest_hash: i.artifact.as_ref().unwrap().manifest_hash.clone(),
        contract_hash: i.contract_hash.clone(),
        approve: true,
        criteria: vec![CriterionReview {
            criterion: 0,
            passed: true,
            evidence: "Compared the received bytes and manifest against the original criterion"
                .into(),
        }],
        findings: vec![],
    };
    let enqueue_review = |node: &mut NodeStore| {
        worker::handle(
            node,
            &key,
            WorkerRequest::Apply {
                token: order.credential.clone(),
                operation_id: id(),
                command: Box::new(review_command.clone()),
            },
        )
    };
    assert!(
        enqueue_review(&mut node).is_err(),
        "review cannot precede complete prepared inputs"
    );
    assert!(
        artifact_review::stage(&mut node, &order, &worktree, &key)
            .unwrap()
            .is_none()
    );
    let count = |node: &NodeStore| {
        node.connection.query_row("SELECT count(*) FROM events WHERE json_extract(wire,'$.record.payload.command.method')='fetch_artifact_chunk'", [], |r| r.get::<_, u64>(0)).unwrap()
    };
    // One binding plus at most sixteen requests in this staging cycle.
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        17
    );
    drop(node);
    let mut node = NodeStore::open(&dir, "other", &owner.did(), grant, &key).unwrap();
    let mut prepared = None;
    for _ in 0..12 {
        node_once(&mut node, &broker).await.unwrap();
        controller_once(&mut x.f.store, &controller).await.unwrap();
        node_once(&mut node, &broker).await.unwrap();
        prepared = artifact_review::stage(&mut node, &order, &worktree, &key).unwrap();
        if prepared.is_some() {
            break;
        }
    }
    let prepared = prepared.expect("all eighteen chunk receipts must reach the reviewer");
    assert_eq!(std::fs::read(prepared.join("report.txt")).unwrap(), bytes);
    let before = count(&node);
    assert_eq!(before, 18);
    assert_eq!(
        artifact_review::stage(&mut node, &order, &worktree, &key).unwrap(),
        Some(prepared.clone())
    );
    assert_eq!(
        count(&node),
        before,
        "completed staging never republishes fetches"
    );
    // Exact scope remains enforced even after the node has fetched its files.
    for (attempt, epoch, integration, name, index) in [
        (x.author.id.clone(), 1, i.id.clone(), "report.txt", 0),
        (reviewer.id.clone(), 2, i.id.clone(), "report.txt", 0),
        (reviewer.id.clone(), 1, id(), "report.txt", 0),
        (reviewer.id.clone(), 1, i.id.clone(), "unassigned.txt", 0),
        (reviewer.id.clone(), 1, i.id.clone(), "report.txt", 999),
    ] {
        assert!(
            x.f.store
                .apply(&request(
                    &order.credential,
                    Mutation::FetchArtifactChunk {
                        attempt,
                        epoch,
                        integration,
                        name: name.into(),
                        index,
                    }
                ))
                .is_err()
        );
    }
    node.enqueue(
        &request(
            &order.credential,
            Mutation::WorkerReady {
                attempt: reviewer.id.clone(),
                epoch: 1,
                session: resource.session,
            },
        ),
        &key,
    )
    .unwrap();
    node_once(&mut node, &broker).await.unwrap();
    controller_once(&mut x.f.store, &controller).await.unwrap();
    let wrong_kind = Mutation::SubmitReview {
        attempt: reviewer.id.clone(),
        epoch: 1,
        integration: i.id.clone(),
        candidate_oid: BASE.into(),
        contract_hash: i.contract_hash.clone(),
        approve: true,
        criteria: vec![CriterionReview {
            criterion: 0,
            passed: true,
            evidence: "Wrong identity type".into(),
        }],
        findings: vec![],
    };
    assert!(
        x.f.store
            .apply(&request(&order.credential, wrong_kind))
            .is_err()
    );
    let mut wrong_hash = review_command.clone();
    if let Mutation::SubmitArtifactReview { manifest_hash, .. } = &mut wrong_hash {
        *manifest_hash = digest(b"wrong manifest");
    }
    assert!(
        x.f.store
            .apply(&request(&order.credential, wrong_hash))
            .is_err()
    );
    // The local API rehashes prepared inputs before sending a review.
    let input = prepared.join("report.txt");
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&input, b"tampered").unwrap();
    assert!(enqueue_review(&mut node).is_err());
    std::fs::write(&input, &bytes).unwrap();
    let submitted = enqueue_review(&mut node).unwrap();
    node_once(&mut node, &broker).await.unwrap();
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "awaiting_review"
    );
    controller_once(&mut x.f.store, &controller).await.unwrap();
    node_once(&mut node, &broker).await.unwrap();
    let receipt = worker::handle(
        &mut node,
        &key,
        WorkerRequest::Receipt {
            token: order.credential,
            attempt: reviewer.id.clone(),
            epoch: 1,
            event: text(&submitted, "event"),
        },
    )
    .unwrap();
    assert_eq!(receipt["state"], "controller_applied");
    crate::control::coordinator::collect(&mut x.f.store).unwrap();
    let notices = || {
        x.f.store.connection.query_row(
        "SELECT count(*) FROM coordinator_notices WHERE kind='submit_artifact_review' AND aggregate_id=?1 AND state='pending'",
        [&reviewer.id], |r| r.get::<_, u64>(0),
    ).unwrap()
    };
    assert_eq!(
        notices(),
        1,
        "artifact review must wake the assigned supervisor"
    );
    assert_eq!(
        verification::integration(&x.f.store.connection, &i.id)
            .unwrap()
            .state,
        "verified"
    );
    x.apply();
    assert_eq!(x.advance().unwrap().state, "applied");
    assert_eq!(
        document::<Task>(&x.f.store.connection, "tasks", &x.author.task)
            .unwrap()
            .state,
        "completed"
    );
    assert_eq!(
        document::<Run>(&x.f.store.connection, "runs", &x.f.run)
            .unwrap()
            .integration_oid,
        BASE
    );
}

#[test]
fn artifact_pipeline_node_schema_upgrade_preserves_jobs_and_pending_signed_events() {
    let (f, author, token, mut node) = fixture();
    let upload = put(&token, &author, chunks("proof.txt", b"evidence").remove(0));
    node.enqueue(&upload, &f.node_key).unwrap();
    let doc: String = node
        .connection
        .query_row("SELECT doc FROM jobs", [], |r| r.get(0))
        .unwrap();
    let wire: String = node
        .connection
        .query_row("SELECT wire FROM events", [], |r| r.get(0))
        .unwrap();
    node.connection
        .execute_batch("DROP TABLE artifact_staging; DROP TABLE registration_recoveries; PRAGMA user_version=6;")
        .unwrap();
    drop(node);
    let node = node_store(&f);
    assert_eq!(
        node.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        8
    );
    assert_eq!(
        node.connection
            .query_row("SELECT doc FROM jobs", [], |r| r.get::<_, String>(0))
            .unwrap(),
        doc
    );
    assert_eq!(
        node.connection
            .query_row("SELECT wire FROM events WHERE state='pending'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
        wire
    );
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM artifact_staging", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}
