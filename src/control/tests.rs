use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};

use serde_json::{Value, json};

use super::identity::*;
use super::model::*;
use super::store::*;

const BASE: &str = "1234567890123456789012345678901234567890";

fn policy() -> Policy {
    Policy {
        max_active: 2,
        reserved_reviewers: 1,
        max_attempts: 3,
        duration_secs: 3600,
        require_review: true,
        profiles: BTreeMap::from([(
            "auto".into(),
            Profile {
                runtime: "codex".into(),
                argv: vec![],
                limit: 2,
            },
        )]),
        checks: BTreeMap::from([("test".into(), vec!["cargo".into(), "test".into()])]),
    }
}
fn contract() -> Contract {
    Contract {
        title: "Fix parser".into(),
        instructions: "Make the parser pass the corpus".into(),
        criteria: vec!["No regressions".into()],
        kind: "code".into(),
        checks: vec!["test".into()],
        memory_topics: vec!["parser".into()],
    }
}
fn request(token: &str, command: Mutation) -> Request {
    Request {
        token: token.into(),
        operation_id: id(),
        command,
    }
}
fn call(s: &mut Store, token: &str, command: Mutation) -> Value {
    s.apply(&request(token, command)).unwrap()
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap().into()
}
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    owner: String,
    run: String,
    node: String,
    supervisor: String,
    node_key: Identity,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::initialize(&dir.path().join("state")).unwrap();
        let owner = std::fs::read_to_string(store.dir.join("admin.token")).unwrap();
        let key = Identity::generate().unwrap();
        let node = text(
            &call(
                &mut store,
                &owner,
                Mutation::EnrollNode {
                    id: "local".into(),
                    key: key.did(),
                    max_active: 2,
                },
            ),
            "credential",
        );
        let run = text(
            &call(
                &mut store,
                &owner,
                Mutation::CreateRun {
                    project: "demo".into(),
                    repo: "/repo".into(),
                    base_oid: BASE.into(),
                    objective: "Improve parser".into(),
                    policy: policy(),
                },
            ),
            "id",
        );
        let supervisor = text(
            &call(
                &mut store,
                &owner,
                Mutation::BindSupervisor {
                    run: run.clone(),
                    expected_revision: 1,
                },
            ),
            "credential",
        );
        Self {
            dir,
            store,
            owner,
            run,
            node,
            supervisor,
            node_key: key,
        }
    }
    fn task(&mut self) -> String {
        let r: Run = document(&self.store.connection, "runs", &self.run).unwrap();
        text(
            &call(
                &mut self.store,
                &self.supervisor,
                Mutation::CreateTask {
                    run: self.run.clone(),
                    expected_revision: r.revision,
                    contract: contract(),
                    dependencies: vec![],
                },
            ),
            "id",
        )
    }
    fn dispatch(&mut self, task: &str) -> (Attempt, String) {
        let revision = document::<Task>(&self.store.connection, "tasks", task)
            .unwrap()
            .revision;
        let a: Attempt = serde_json::from_value(call(
            &mut self.store,
            &self.supervisor,
            Mutation::Dispatch {
                task: task.into(),
                expected_revision: revision,
                node: "local".into(),
                profile: "auto".into(),
            },
        ))
        .unwrap();
        let effects = self.store.pending(&self.node, "local").unwrap();
        let secret = text(
            &effects
                .iter()
                .find(|e| e.payload["attempt"]["id"] == a.id)
                .unwrap()
                .payload,
            "credential",
        );
        (a, secret)
    }
    fn ready(&mut self, a: &Attempt, token: &str) -> Resource {
        let resource = Resource {
            repo_common_dir: "/repo/.git".into(),
            worktree: format!("/worktrees/{}", a.id),
            branch: format!("hs/{}/{}", a.run, a.id),
            workspace: "w1".into(),
            pane: format!("w1:{}", a.id),
            terminal: format!("term-{}", a.id),
            session: id(),
            runtime: "codex".into(),
            mailer_agent: format!("worker-{}", a.id),
            mailer_epoch: 1,
        };
        call(
            &mut self.store,
            &self.node,
            Mutation::BindResource {
                attempt: a.id.clone(),
                epoch: 1,
                resource: resource.clone(),
            },
        );
        call(
            &mut self.store,
            token,
            Mutation::WorkerReady {
                attempt: a.id.clone(),
                epoch: 1,
                session: resource.session.clone(),
            },
        );
        resource
    }
}

#[test]
fn transactional_dispatch_idempotency_survives_restart_and_content_collision() {
    let mut f = Fixture::new();
    let task = f.task();
    let r = request(
        &f.supervisor,
        Mutation::Dispatch {
            task: task.clone(),
            expected_revision: 1,
            node: "local".into(),
            profile: "auto".into(),
        },
    );
    let first = f.store.apply(&r).unwrap();
    drop(f.store);
    f.store = Store::open(&f.dir.path().join("state")).unwrap();
    assert_eq!(first, f.store.apply(&r).unwrap());
    assert_eq!(f.store.pending(&f.node, "local").unwrap().len(), 1);
    let mut different = r;
    different.command = Mutation::Dispatch {
        task,
        expected_revision: 2,
        node: "local".into(),
        profile: "auto".into(),
    };
    assert!(
        f.store
            .apply(&different)
            .unwrap_err()
            .to_string()
            .contains("different content")
    );
}

#[test]
fn concurrent_dispatch_reserves_capacity_once_and_keeps_review_slot() {
    let mut f = Fixture::new();
    let a = f.task();
    let b = f.task();
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = [a, b]
        .into_iter()
        .map(|task| {
            let path = f.store.dir.clone();
            let token = f.supervisor.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&path).unwrap();
                barrier.wait();
                s.apply(&request(
                    &token,
                    Mutation::Dispatch {
                        task,
                        expected_revision: 1,
                        node: "local".into(),
                        profile: "auto".into(),
                    },
                ))
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(f.store.pending(&f.node, "local").unwrap().len(), 1);
}

#[test]
fn outbox_failure_rolls_back_attempt_task_capability_and_operation() {
    let mut f = Fixture::new();
    let task = f.task();
    let before: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM principals", [], |r| r.get(0))
        .unwrap();
    f.store.connection.execute_batch("CREATE TRIGGER fail_outbox BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT,'disk full'); END;").unwrap();
    let req = request(
        &f.supervisor,
        Mutation::Dispatch {
            task: task.clone(),
            expected_revision: 1,
            node: "local".into(),
            profile: "auto".into(),
        },
    );
    assert!(f.store.apply(&req).is_err());
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &task)
            .unwrap()
            .state,
        "planned"
    );
    let after: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM principals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    assert!(f.store.pending(&f.node, "local").unwrap().is_empty());
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_outbox;")
        .unwrap();
    assert!(f.store.apply(&req).is_ok());
}

#[test]
fn cycles_and_unsatisfied_dependencies_cannot_dispatch() {
    let mut f = Fixture::new();
    let a = f.task();
    let b = f.task();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AddDependency {
            task: a.clone(),
            expected_revision: 1,
            predecessor: b.clone(),
        },
    );
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::AddDependency {
                    task: b.clone(),
                    expected_revision: 1,
                    predecessor: a.clone()
                }
            ))
            .is_err()
    );
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::Dispatch {
                    task: a,
                    expected_revision: 2,
                    node: "local".into(),
                    profile: "auto".into()
                }
            ))
            .is_err()
    );
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &b)
            .unwrap()
            .revision,
        1
    );
}

#[test]
fn supervisor_rotation_fences_old_credentials_even_for_replay() {
    let mut f = Fixture::new();
    let old = request(
        &f.supervisor,
        Mutation::CreateTask {
            run: f.run.clone(),
            expected_revision: 2,
            contract: contract(),
            dependencies: vec![],
        },
    );
    f.store.apply(&old).unwrap();
    call(
        &mut f.store,
        &f.owner,
        Mutation::BindSupervisor {
            run: f.run.clone(),
            expected_revision: 3,
        },
    );
    assert!(f.store.apply(&old).is_err());
    assert!(f.store.snapshot(&f.supervisor, &f.run).is_err());
}

#[test]
fn worker_needs_exact_binding_then_result_is_not_an_approval_or_free_slot() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    assert!(
        f.store
            .apply(&request(
                &token,
                Mutation::WorkerReady {
                    attempt: a.id.clone(),
                    epoch: 1,
                    session: id()
                }
            ))
            .is_err()
    );
    let resource = f.ready(&a, &token);
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::WorkerReady {
                    attempt: a.id.clone(),
                    epoch: 1,
                    session: resource.session
                }
            ))
            .is_err()
    );
    let submit = request(
        &token,
        Mutation::SubmitResult {
            attempt: a.id.clone(),
            epoch: 1,
            contract_hash: a.contract_hash.clone(),
            commit: Some(BASE.into()),
            artifacts: BTreeMap::new(),
            summary: "Candidate ready".into(),
        },
    );
    let result = f.store.apply(&submit).unwrap();
    assert_eq!(result, f.store.apply(&submit).unwrap());
    assert_eq!(
        document::<Task>(&f.store.connection, "tasks", &task)
            .unwrap()
            .state,
        "awaiting_review"
    );
    let second = f.task();
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::Dispatch {
                    task: second,
                    expected_revision: 1,
                    node: "local".into(),
                    profile: "auto".into()
                }
            ))
            .is_err()
    );
    call(
        &mut f.store,
        &f.node,
        Mutation::ObserveStopped {
            attempt: a.id,
            epoch: 1,
        },
    );
    assert!(f.store.apply(&submit).is_err());
}

#[test]
fn cancellation_fences_workers_without_freeing_unknown_processes() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    f.ready(&a, &token);
    let revision = document::<Run>(&f.store.connection, "runs", &f.run)
        .unwrap()
        .revision;
    let cancel = call(
        &mut f.store,
        &f.supervisor,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: revision,
        },
    );
    assert_eq!(cancel["state"], "cancelling");
    assert!(f.store.snapshot(&token, &f.run).is_err());
    let active: bool = f
        .store
        .connection
        .query_row("SELECT active FROM attempts WHERE id=?1", [&a.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(active);
    assert!(
        f.store
            .apply(&request(
                &f.node,
                Mutation::ObserveStopped {
                    attempt: a.id.clone(),
                    epoch: 1
                }
            ))
            .is_err()
    );
    call(
        &mut f.store,
        &f.node,
        Mutation::ObserveStopped {
            attempt: a.id,
            epoch: 2,
        },
    );
    assert_eq!(
        document::<Run>(&f.store.connection, "runs", &f.run)
            .unwrap()
            .state,
        "cancelled"
    );
}

#[test]
fn memory_is_curated_versioned_scoped_and_budgeted() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let proposal = call(
        &mut f.store,
        &token,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "Use UTF-8 byte offsets".into(),
            source_attempt: Some(a.id),
            evidence: vec![format!("git:{BASE}")],
        },
    );
    assert_eq!(
        f.store
            .memory(&token, &f.run, &["parser".into()], 32000)
            .unwrap()["topics"],
        json!({})
    );
    let approve = Mutation::ApproveMemory {
        proposal: text(&proposal, "id"),
        expected_revision: 0,
    };
    assert!(f.store.apply(&request(&token, approve.clone())).is_err());
    call(&mut f.store, &f.supervisor, approve);
    let memory = f
        .store
        .memory(&token, &f.run, &["parser".into()], 32000)
        .unwrap();
    assert_eq!(memory["topics"]["parser"]["revision"], 1);
    assert_eq!(
        f.store
            .memory(&token, &f.run, &["parser".into()], 3)
            .unwrap()["omitted"],
        json!(["parser"])
    );
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::WithdrawMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            expected_revision: 1,
        },
    );
    assert_eq!(
        f.store
            .memory(&token, &f.run, &["parser".into()], 32000)
            .unwrap()["topics"],
        json!({})
    );
}

#[test]
fn signed_records_require_correct_author_hash_time_and_signature() {
    let owner = Identity::generate().unwrap();
    let node = Identity::generate().unwrap();
    let now = jiff::Timestamp::now().as_second();
    let grant = owner.delegation("node1", &node.did(), 1, now + 60).unwrap();
    grant.verify(now).unwrap();
    assert!(grant.verify(now + 60).is_err());
    let mut forged = grant.clone();
    forged.record.payload["node"] = json!("node2");
    assert!(forged.verify(now).is_err());
    forged.content_hash = digest(&serde_json::to_vec(&forged.record).unwrap());
    assert!(forged.verify(now).is_err());
    let mut substituted = grant;
    substituted.record.author = node.did();
    assert!(substituted.verify(now).is_err());
}

#[test]
fn schema_future_version_is_refused_without_overwriting_data() {
    let f = Fixture::new();
    f.store
        .connection
        .pragma_update(None, "user_version", 99)
        .unwrap();
    assert!(Store::open(&f.store.dir).is_err());
    assert_eq!(
        f.store.owner().unwrap(),
        Identity::load(&f.store.dir.join("owner.key"))
            .unwrap()
            .did()
    );
}

fn node_store(f: &Fixture) -> super::node::NodeStore {
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let grant = owner
        .delegation(
            "local",
            &f.node_key.did(),
            1,
            jiff::Timestamp::now().as_second() + 3600,
        )
        .unwrap();
    super::node::NodeStore::open(
        &f.dir.path().join("node"),
        "local",
        &owner.did(),
        grant,
        &f.node_key,
    )
    .unwrap()
}

#[test]
fn node_persists_deduplicates_and_cancellation_prevents_delayed_launch() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, _) = f.dispatch(&task);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let execute = f.store.pending(&f.node, "local").unwrap().remove(0);
    let wire = f.store.prepare_wire(&execute.id, &owner).unwrap();
    assert_eq!(
        serde_json::to_string(&wire).unwrap(),
        serde_json::to_string(&f.store.prepare_wire(&execute.id, &owner).unwrap()).unwrap()
    );
    let mut node = node_store(&f);
    let revision = document::<Run>(&f.store.connection, "runs", &f.run)
        .unwrap()
        .revision;
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: revision,
        },
    );
    let cancel = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "cancel")
        .unwrap();
    let cancel = f.store.prepare_wire(&cancel.id, &owner).unwrap();
    node.receive(&cancel).unwrap();
    node.receive(&wire).unwrap();
    node.receive(&wire).unwrap();
    let (state, epoch): (String, u64) = node
        .connection
        .query_row("SELECT state,epoch FROM jobs WHERE id=?1", [&a.id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((state, epoch), ("cancelling".into(), 2));
    let command_state: String = node
        .connection
        .query_row(
            "SELECT state FROM commands WHERE id=?1",
            [&wire.record.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(command_state, "superseded");
}

#[test]
fn controller_authenticates_origin_before_persisting_or_mutating() {
    let mut f = Fixture::new();
    let task = f.task();
    let (a, _) = f.dispatch(&task);
    let mut node = node_store(&f);
    let resource = Resource {
        repo_common_dir: "/repo/.git".into(),
        worktree: "/wt".into(),
        branch: format!("hs/{}/{}", a.run, a.id),
        workspace: "w1".into(),
        pane: "w1:p1".into(),
        terminal: "term".into(),
        session: id(),
        runtime: "codex".into(),
        mailer_agent: "worker1".into(),
        mailer_epoch: 1,
    };
    let req = request(
        &f.node,
        Mutation::BindResource {
            attempt: a.id.clone(),
            epoch: 1,
            resource,
        },
    );
    node.enqueue(&req, &f.node_key).unwrap();
    let event = node.pending().unwrap().remove(0).1;
    let mut forged = event.clone();
    forged.record.epoch = 2;
    forged = f.node_key.sign(forged.record).unwrap();
    assert!(f.store.receive(&forged).is_err());
    let count: i64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM inbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    f.store
        .connection
        .execute_batch(
            "CREATE TRIGGER no_inbox BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'full'); END;",
        )
        .unwrap();
    assert!(f.store.receive(&event).is_err());
    assert_eq!(
        document::<Attempt>(&f.store.connection, "attempts", &a.id)
            .unwrap()
            .state,
        "reserved"
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER no_inbox;")
        .unwrap();
    let first = f.store.receive(&event).unwrap();
    assert_eq!(first, f.store.receive(&event).unwrap());
    assert_eq!(
        document::<Attempt>(&f.store.connection, "attempts", &a.id)
            .unwrap()
            .state,
        "awaiting_ready"
    );
}

struct NatsServer {
    child: std::process::Child,
    dir: tempfile::TempDir,
    url: String,
}
impl Drop for NatsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl NatsServer {
    fn start(owner: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let policy =
            super::broker_policy::policy(owner, &["local".into(), "other".into()]).unwrap();
        let config = json!({
            "listen":format!("127.0.0.1:{port}"),
            "jetstream":{"store_dir":dir.path().join("jetstream"),"max_file_store":2147483648_u64,"max_memory_store":16777216},
            "authorization":{"users":[
                {"user":"admin","password":"admin-test-password"},
                {"user":"controller","password":"controller-test-password","permissions":policy["controller"]["permissions"]},
                {"user":"node","password":"node-test-password","permissions":policy["nodes"]["local"]["permissions"]},
                {"user":"other","password":"other-test-password","permissions":policy["nodes"]["other"]["permissions"]}
            ]}
        }).to_string();
        let path = dir.path().join("server.conf");
        std::fs::write(&path, config).unwrap();
        let binary = std::env::var_os("NATS_SERVER").unwrap_or_else(|| "nats-server".into());
        let log = std::fs::File::create(dir.path().join("server.log")).unwrap();
        let mut child = std::process::Command::new(binary)
            .arg("-c")
            .arg(path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect(
                "NATS_SERVER or nats-server is required; integration tests never silently skip",
            );
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if child.try_wait().unwrap().is_some() || std::time::Instant::now() > until {
                panic!(
                    "NATS failed: {}",
                    std::fs::read_to_string(dir.path().join("server.log")).unwrap()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Self {
            child,
            dir,
            url: format!("nats://127.0.0.1:{port}"),
        }
    }
    fn config(&self, name: &str) -> super::transport::BrokerConfig {
        let path = self.dir.path().join(format!("{name}.json"));
        secret_file(
            &path,
            serde_json::to_string(&json!({"user":name,"password":format!("{name}-test-password")}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        super::transport::BrokerConfig {
            url: self.url.clone(),
            credentials: path,
            private_network_plaintext: true,
        }
    }
}

#[tokio::test]
async fn real_jetstream_offline_node_restart_and_ack_after_persistence() {
    use super::transport::*;
    use futures_util::StreamExt;
    let mut f = Fixture::new();
    let task = f.task();
    f.dispatch(&task);
    let server = NatsServer::start(&f.store.owner().unwrap());
    let admin = Broker::connect(
        &server.config("admin"),
        &f.store.owner().unwrap(),
        crate::control::broker_policy::Endpoint::Provisioner,
    )
    .await
    .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    assert_eq!(controller_once(&mut f.store, &admin).await.unwrap(), 1);
    // Consumer existed before the offline node, so its durable history survives reconnect.
    let node_broker = Broker::connect(
        &server.config("node"),
        &f.store.owner().unwrap(),
        crate::control::broker_policy::Endpoint::Node("local"),
    )
    .await
    .unwrap();
    let mut node = node_store(&f);
    node.connection
        .execute_batch(
            "CREATE TRIGGER full BEFORE INSERT ON jobs BEGIN SELECT RAISE(ABORT,'full'); END;",
        )
        .unwrap();
    let mut consumer = node_broker.consumer("local").await.unwrap();
    let mut messages = consumer
        .fetch()
        .max_messages(1)
        .expires(std::time::Duration::from_secs(2))
        .messages()
        .await
        .unwrap();
    let msg = messages.next().await.unwrap().unwrap();
    let record = decode(&msg.payload).unwrap();
    assert!(node.receive(&record).is_err());
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 1);
    let count: i64 = node
        .connection
        .query_row("SELECT count(*) FROM commands", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    node.connection.execute_batch("DROP TRIGGER full;").unwrap();
    node.receive(&record).unwrap();
    msg.double_ack().await.unwrap();
    assert_eq!(consumer.info().await.unwrap().num_ack_pending, 0);
    drop(node);
    let mut node = node_store(&f);
    node.receive(&record).unwrap();
    let count: i64 = node
        .connection
        .query_row("SELECT count(*) FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    // The durable receipt has created a reserved intent, never a model execution claim.
    let state: String = node
        .connection
        .query_row("SELECT state FROM jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "reserved");
}

#[tokio::test]
async fn real_broker_acl_blocks_node_publishing_controller_commands() {
    use super::transport::*;
    let f = Fixture::new();
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = Broker::connect(
        &server.config("admin"),
        &owner,
        crate::control::broker_policy::Endpoint::Provisioner,
    )
    .await
    .unwrap();
    admin
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let broker = Broker::connect(
        &server.config("node"),
        &owner,
        crate::control::broker_policy::Endpoint::Node("local"),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        broker
            .context
            .publish(broker.subject("local").unwrap(), "unauthorized".into())
            .await?
            .await
    })
    .await;
    assert!(!matches!(result, Ok(Ok(_))));
    let mut stream = admin.context.get_stream(&admin.stream).await.unwrap();
    assert_eq!(stream.info().await.unwrap().state.messages, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_api_and_mcp_share_authorization_and_incomplete_clients_do_not_block() {
    use super::api::{self, ApiRequest};
    let f = Fixture::new();
    let state = f.store.dir.clone();
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let path = state.clone();
    let server = tokio::spawn(async move {
        api::serve(path, async {
            let _ = stop.await;
        })
        .await
    });
    let mut ready = false;
    for _ in 0..100 {
        if state.join("control.sock").exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(ready);
    let half_open = std::os::unix::net::UnixStream::connect(state.join("control.sock")).unwrap();
    std::io::Write::write_all(&mut &half_open, b"{\"op\":").unwrap();
    let run = f.run.clone();
    let token = f.supervisor.clone();
    let checked = tokio::task::spawn_blocking(move || {
        let data = api::call(
            &state,
            &ApiRequest::Snapshot {
                token: token.clone(),
                run: run.clone(),
            },
        )
        .unwrap();
        assert_eq!(data["run"]["id"], run);
        assert!(
            api::call(
                &state,
                &ApiRequest::Snapshot {
                    token: "x".repeat(40),
                    run: run.clone()
                }
            )
            .is_err()
        );
        let lines = format!(
            "{}\n{}\n{}\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"supervisor_snapshot","arguments":{"run":run}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"supervisor_context","arguments":{"run":run}}})
        );
        let mut output = Vec::new();
        api::mcp(&state, &token, lines.as_bytes(), &mut output).unwrap();
        let results: Vec<Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[1]["result"]["isError"], false);
        assert_eq!(results[2]["result"]["isError"], false);
        let context: Value =
            serde_json::from_str(results[2]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(context["nodes"][0]["id"], "local");
        assert!(
            context["instructions"]
                .as_str()
                .unwrap()
                .contains("request_verification")
        );
        assert!(!String::from_utf8(output).unwrap().contains(&token));
    });
    checked.await.unwrap();
    shutdown.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(half_open);
    assert!(!f.store.dir.join("control.sock").exists());
}

/// Real Git with a fake Herdr boundary: resource creation succeeds but its reply
/// is lost. A fresh NodeStore must reconcile the same checkout without a second call.
#[test]
fn executor_reconciles_lost_creation_reply_and_fences_cancelled_work() {
    use crate::runner::{Cmd, Output, RealRunner, Runner};
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    struct Resources {
        creates: Cell<usize>,
        repo: PathBuf,
        path: PathBuf,
        produce: bool,
        terminal: Cell<u8>,
    }
    impl Runner for Resources {
        fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
            if cmd.program != "fake-herdr" {
                return RealRunner.run(cmd);
            }
            let args = &cmd.args;
            let contains = |s: &str| args.iter().any(|a| a == s);
            if contains("create") {
                self.creates.set(self.creates.get() + 1);
                if self.produce {
                    let at = |s: &str| args[args.iter().position(|a| a == s).unwrap() + 1].clone();
                    let out = RealRunner.run(
                        &Cmd::new("git", Duration::from_secs(10))
                            .cwd(&self.repo)
                            .args([
                                "worktree".into(),
                                "add".into(),
                                "-b".into(),
                                at("--branch"),
                                at("--path"),
                                at("--base"),
                            ]),
                    )?;
                    assert!(out.success(), "{}", out.error_text());
                }
                return Ok(Output {
                    timed_out: true,
                    ..Default::default()
                });
            }
            let result = if contains("workspace") {
                json!({"workspaces":[{"workspace_id":"w1","worktree":{"checkout_path":self.path,"is_linked_worktree":true}}]})
            } else if contains("process-info") {
                json!({"process_info":{"shell_pid":1,"foreground_process_group_id":1,"foreground_processes":[{"pid":1}]}})
            } else if contains("pane") {
                json!({"panes":[{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","terminal_id":format!("term{}",self.terminal.get()),"cwd":self.path}]})
            } else if contains("agent") {
                json!({"agents":[]})
            } else {
                panic!("unexpected Herdr call")
            };
            Ok(Output {
                code: Some(0),
                stdout: json!({"result":result}).to_string(),
                ..Default::default()
            })
        }
        fn socket_request(&self, _: &Path, _: &str, _: Duration) -> anyhow::Result<String> {
            panic!("no direct sockets")
        }
        fn run_foreground(
            &self,
            _: &Cmd,
            _: &mut dyn FnMut() -> bool,
        ) -> anyhow::Result<Option<i32>> {
            panic!("no models")
        }
    }
    for produce in [true, false] {
        let mut f = Fixture::new();
        let repo = f.dir.path().join("repo with spaces");
        std::fs::create_dir(&repo).unwrap();
        for argv in [
            vec!["init", "--quiet"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "base",
            ],
        ] {
            let result = RealRunner
                .run(
                    &Cmd::new("git", Duration::from_secs(10))
                        .cwd(&repo)
                        .args(argv),
                )
                .unwrap();
            assert!(result.success(), "{}", result.error_text());
        }
        let base = RealRunner
            .run(
                &Cmd::new("git", Duration::from_secs(10))
                    .cwd(&repo)
                    .args(["rev-parse", "HEAD"]),
            )
            .unwrap()
            .stdout
            .trim()
            .to_string();
        let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        run.base_oid = base.clone();
        run.integration_oid = base;
        f.store
            .connection
            .execute(
                "UPDATE runs SET doc=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&run).unwrap(), f.run],
            )
            .unwrap();
        let task = f.task();
        let (a, _) = f.dispatch(&task);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
            .unwrap();
        let root = f.dir.path().join("worktrees");
        let runner = Resources {
            creates: Cell::new(0),
            repo: repo.clone(),
            path: root.join(&a.id),
            produce,
            terminal: Cell::new(1),
        };
        let config = super::executor::ExecutorConfig {
            base_remotes: BTreeMap::new(),
            repositories: BTreeMap::from([("/repo".into(), repo)]),
            worktree_root: root,
            herdr_binary: "fake-herdr".into(),
            herdr_socket: f.dir.path().join("herdr.sock"),
        };
        assert_eq!(
            super::executor::prepare(&mut node, &config, &a.id, &runner).is_ok(),
            produce
        );
        drop(node);
        let mut node = node_store(&f);
        assert_eq!(
            super::executor::prepare(&mut node, &config, &a.id, &runner).is_ok(),
            produce
        );
        assert_eq!(
            runner.creates.get(),
            1,
            "lost replies never duplicate creation"
        );
        if produce {
            runner.terminal.set(2);
            assert!(
                super::executor::prepare(&mut node, &config, &a.id, &runner).is_err(),
                "pane reuse cannot substitute terminal identity"
            );
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
        let effect = f
            .store
            .pending(&f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "cancel")
            .unwrap();
        let cancel = f.store.prepare_wire(&effect.id, &owner).unwrap();
        let lease = super::api::lock(&node.dir, "execution.lock").unwrap();
        assert!(
            node.receive(&cancel).is_err(),
            "cancellation cannot race external mutation lease"
        );
        drop(lease);
        node.receive(&cancel).unwrap();
        assert!(super::executor::prepare(&mut node, &config, &a.id, &runner).is_err());
        assert_eq!(runner.creates.get(), 1);
    }
}

#[test]
fn worker_boundary_scopes_context_mutations_receipts_and_revocation() {
    use super::worker::{WorkerRequest, handle};
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    let mut node = node_store(&f);
    node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
        .unwrap();
    let context = handle(
        &mut node,
        &f.node_key,
        WorkerRequest::Context {
            token: token.clone(),
            attempt: a.id.clone(),
            epoch: a.epoch,
        },
    )
    .unwrap();
    assert_eq!(context["contract"]["title"], "Fix parser");
    assert!(!context.to_string().contains(&token));
    assert!(
        handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Context {
                token: f.node.clone(),
                attempt: a.id.clone(),
                epoch: a.epoch
            }
        )
        .is_err()
    );
    assert!(
        handle(
            &mut node,
            &f.node_key,
            WorkerRequest::Context {
                token: token.clone(),
                attempt: a.id.clone(),
                epoch: 2
            }
        )
        .is_err()
    );
    let decision = WorkerRequest::Apply {
        token: token.clone(),
        operation_id: id(),
        command: Box::new(Mutation::Pause {
            run: f.run.clone(),
            expected_revision: 1,
        }),
    };
    assert!(handle(&mut node, &f.node_key, decision).is_err());
    let proposal = Mutation::ProposeMemory {
        run: f.run.clone(),
        topic: "testing".into(),
        text: "Use corpus fixtures".into(),
        source_attempt: Some(a.id.clone()),
        evidence: vec!["test-result:123".into()],
    };
    let operation = id();
    let apply = || WorkerRequest::Apply {
        token: token.clone(),
        operation_id: operation.clone(),
        command: Box::new(proposal.clone()),
    };
    let queued = handle(&mut node, &f.node_key, apply()).unwrap();
    assert_eq!(queued["state"], "queued");
    assert_eq!(handle(&mut node, &f.node_key, apply()).unwrap(), queued);
    let event = text(&queued, "event");
    let receipt = || WorkerRequest::Receipt {
        token: token.clone(),
        attempt: a.id.clone(),
        epoch: a.epoch,
        event: event.clone(),
    };
    assert_eq!(
        handle(&mut node, &f.node_key, receipt()).unwrap()["state"],
        "pending"
    );
    let (local_id, wire) = node.pending().unwrap().remove(0);
    node.published(&local_id).unwrap();
    assert_eq!(
        handle(&mut node, &f.node_key, receipt()).unwrap()["state"],
        "broker_stored"
    );
    let response = f.store.receive(&wire).unwrap();
    let ack = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "receipt")
        .unwrap();
    let ack = f.store.prepare_wire(&ack.id, &owner).unwrap();
    node.receive(&ack).unwrap();
    node.receive(&ack).unwrap();
    let observed = handle(&mut node, &f.node_key, receipt()).unwrap();
    assert_eq!(observed["state"], "controller_applied");
    assert_eq!(observed["result"], response);
    // A delayed publisher acknowledgement must not downgrade a controller receipt.
    node.published(&local_id).unwrap();
    assert_eq!(
        handle(&mut node, &f.node_key, receipt()).unwrap()["state"],
        "controller_applied"
    );
    drop(node);
    let mut node = node_store(&f);
    assert_eq!(
        handle(&mut node, &f.node_key, receipt()).unwrap()["result"],
        response
    );
    let mut collision = wire.clone();
    collision.record.id = wire.record.id.clone();
    collision.record.payload["operation_id"] = json!(id());
    let collision = f.node_key.sign(collision.record).unwrap();
    assert!(f.store.receive(&collision).is_err());
    let revision = document::<Run>(&f.store.connection, "runs", &f.run)
        .unwrap()
        .revision;
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: revision,
        },
    );
    let cancel = f
        .store
        .pending(&f.node, "local")
        .unwrap()
        .into_iter()
        .find(|e| e.kind == "cancel")
        .unwrap();
    node.receive(&f.store.prepare_wire(&cancel.id, &owner).unwrap())
        .unwrap();
    assert!(handle(&mut node, &f.node_key, apply()).is_err());
    assert!(handle(&mut node, &f.node_key, receipt()).is_err());
    assert_eq!(
        f.store.receive(&wire).unwrap(),
        response,
        "exact accepted broker replay remains acknowledgeable after worker retirement"
    );
    let mut new_event = wire.record.clone();
    new_event.id = id();
    assert!(
        f.store
            .receive(&f.node_key.sign(new_event).unwrap())
            .is_err(),
        "retired credential cannot authorize a new envelope"
    );
}

#[test]
fn native_wrapper_child() {
    let Ok(state) = std::env::var("HS_TEST_WRAPPER_STATE") else {
        return;
    };
    let attempt = std::env::var("HS_TEST_WRAPPER_ATTEMPT").unwrap();
    super::runtime::run_owned(std::path::Path::new(&state), &attempt).unwrap();
}

struct WrapperChild(std::process::Child);
impl WrapperChild {
    fn wait(&mut self) -> std::process::ExitStatus {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                std::time::Instant::now() < until,
                "test wrapper failed to exit"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Drop for WrapperChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn three_native_launchers_bind_exact_sessions_before_idempotent_bootstrap() {
    use super::executor::{CheckoutPlan, PaneBinding};
    use super::runtime::{LaunchPlan, RuntimeConfig};
    use crate::runner::{Cmd, Output, RealRunner, Runner};
    use std::cell::RefCell;
    use std::io::{BufRead, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    struct RuntimeHerdr {
        path: String,
        runtime: String,
        session: RefCell<String>,
        launches: RefCell<Vec<String>>,
    }
    impl Runner for RuntimeHerdr {
        fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
            if cmd.program != "mock-herdr" {
                return RealRunner.run(cmd);
            }
            let has = |v: &str| cmd.args.iter().any(|a| a == v);
            let result = if has("workspace") {
                json!({"workspaces":[{"workspace_id":"w","worktree":{"checkout_path":self.path,"is_linked_worktree":true}}]})
            } else if has("process-info") {
                json!({"process_info":{"shell_pid":7,"foreground_process_group_id":7,"foreground_processes":[{"pid":7}]}})
            } else if has("run") {
                self.launches
                    .borrow_mut()
                    .push(cmd.args.last().unwrap().clone());
                return Ok(Output {
                    timed_out: true,
                    ..Default::default()
                });
            } else if has("pane") {
                json!({"panes":[{"pane_id":"p","workspace_id":"w","tab_id":"t","terminal_id":"term","cwd":self.path}]})
            } else if has("agent") {
                if self.session.borrow().is_empty() {
                    json!({"agents":[]})
                } else {
                    json!({"agents":[{"pane_id":"p","workspace_id":"w","tab_id":"t","terminal_id":"term","cwd":self.path,"agent":self.runtime,"agent_session":{"value":*self.session.borrow()}}]})
                }
            } else {
                panic!("unexpected call {cmd:?}")
            };
            Ok(Output {
                code: Some(0),
                stdout: json!({"result":result}).to_string(),
                ..Default::default()
            })
        }
        fn socket_request(&self, _: &Path, _: &str, _: Duration) -> anyhow::Result<String> {
            panic!("no Herdr sockets")
        }
        fn run_foreground(
            &self,
            _: &Cmd,
            _: &mut dyn FnMut() -> bool,
        ) -> anyhow::Result<Option<i32>> {
            panic!("no models")
        }
    }
    for runtime in ["codex", "claude", "pi"] {
        let mut f = Fixture::new();
        let repo = f.dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let g = |args: Vec<String>| {
            let out = RealRunner
                .run(
                    &Cmd::new("git", Duration::from_secs(10))
                        .cwd(&repo)
                        .args(args),
                )
                .unwrap();
            assert!(out.success(), "{}", out.error_text());
            out.stdout.trim().to_string()
        };
        g(vec!["init".into(), "--quiet".into()]);
        g([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "base",
        ]
        .map(str::to_string)
        .to_vec());
        let base = g(vec!["rev-parse".into(), "HEAD".into()]);
        let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        run.policy.profiles.get_mut("auto").unwrap().runtime = runtime.into();
        run.policy.profiles.get_mut("auto").unwrap().argv =
            vec!["--model".into(), "mock-model".into()];
        run.base_oid = base.clone();
        run.integration_oid = base.clone();
        f.store
            .connection
            .execute(
                "UPDATE runs SET doc=?1 WHERE id=?2",
                rusqlite::params![serde_json::to_string(&run).unwrap(), f.run],
            )
            .unwrap();
        let task = f.task();
        let (a, worker) = f.dispatch(&task);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
            .unwrap();
        let worktree = f.dir.path().join("checkout with spaces");
        let branch = format!("hs/{}/{}", a.run, a.id);
        g(vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.clone(),
            worktree.to_str().unwrap().into(),
            base.clone(),
        ]);
        let checkpoint = CheckoutPlan {
            repo: repo.clone(),
            common_dir: repo.join(".git"),
            worktree: worktree.clone(),
            branch,
            base_oid: base,
            socket: f.dir.path().join("fake-herdr.sock"),
            herdr_binary: "mock-herdr".into(),
            creation_started: true,
            binding: Some(PaneBinding {
                workspace: "w".into(),
                pane: "p".into(),
                terminal: "term".into(),
            }),
        };
        node.connection
            .execute(
                "UPDATE jobs SET state='prepared',checkpoint=?2 WHERE id=?1",
                rusqlite::params![a.id, serde_json::to_string(&checkpoint).unwrap()],
            )
            .unwrap();
        // A real child records argv/environment and waits for release; no model is contacted.
        let stub = f.dir.path().join("fake runtime");
        std::fs::write(&stub,b"#!/bin/sh\nprintf '%s\\n' \"$HERDR_SUPERVISOR_ATTEMPT\" \"$HERDR_SUPERVISOR_EPOCH\" \"$HERDR_MAILER_STATE\" \"$HERDR_MAILER_CREDENTIAL\" \"$HERDR_MAILER_CONFIG\" \"$@\" > \"$HS_TEST_CAPTURE\"\nwhile [ ! -f \"$HS_TEST_CAPTURE.release\" ]; do sleep 0.02; done\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o700)).unwrap();
        let maildir = f.dir.path().join("mailer");
        private_dir(&maildir).unwrap();
        let admin = f.dir.path().join("admin.token");
        secret_file(&admin, b"mock-admin").unwrap();
        let node_credential = node.dir.join("node.token");
        secret_file(&node_credential, f.node.as_bytes()).unwrap();
        let config = RuntimeConfig {
            supervisor_binary: stub.clone(),
            mailer_state: maildir.clone(),
            mailer_admin_credential: admin,
            node_credential,
            codex_binary: stub.clone(),
            mailer_runtime_binary: stub,
            claude_development_channel: true,
        };
        let listener = std::os::unix::net::UnixListener::bind(maildir.join("daemon.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = stop.clone();
        let server = std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(socket.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let r: Value = serde_json::from_str(&line).unwrap();
                captured.lock().unwrap().push(r.clone());
                let data = match r["op"].as_str().unwrap() {
                    "register" => {
                        json!({"session":{"id":id(),"epoch":1,"agent":r["agent"],"runtime":r["runtime"],"location":r["location"]},"token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})
                    }
                    "capabilities" => json!({"idempotentSend":true,"recipientPrecondition":true}),
                    "send" => {
                        json!({"id":id(),"recipientSession":r["recipientSession"],"recipientEpoch":r["recipientEpoch"]})
                    }
                    _ => panic!("unexpected mailer request"),
                };
                writeln!(socket, "{}", json!({"data":data})).unwrap();
            }
        });
        let runner = RuntimeHerdr {
            path: worktree.to_str().unwrap().into(),
            runtime: runtime.into(),
            session: RefCell::new(String::new()),
            launches: RefCell::new(vec![]),
        };
        assert_eq!(
            super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).unwrap()["state"],
            "launching"
        );
        assert!(super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).is_err());
        assert_eq!(
            runner.launches.borrow().len(),
            1,
            "no second launch after lost reply"
        );
        let capture = f.dir.path().join("invocation");
        let child = || {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "control::tests::native_wrapper_child",
                    "--nocapture",
                ])
                .env("HS_TEST_WRAPPER_STATE", &node.dir)
                .env("HS_TEST_WRAPPER_ATTEMPT", &a.id)
                .env("HS_TEST_CAPTURE", &capture)
                .env("HERDR_MAILER_CONFIG", "wrong-parent")
                .env("HERDR_MAILER_CREDENTIAL", "wrong-parent")
                .env("HERDR_MAILER_STATE", "wrong-parent");
            command
        };
        let mut launched =
            WrapperChild(child().stdout(std::process::Stdio::null()).spawn().unwrap());
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !capture.exists() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !child().output().unwrap().status.success(),
            "wrapper cannot launch twice"
        );
        let invocation = std::fs::read_to_string(&capture).unwrap();
        assert!(!invocation.contains("wrong-parent"));
        assert!(invocation.contains(&a.id));
        assert!(invocation.contains("mock-model"));
        if runtime == "codex" {
            assert!(invocation.contains("--approve-for-me\n--sandbox\nworkspace-write"));
        }
        if runtime == "claude" {
            assert!(invocation.contains("--development-channel"));
        }
        let plan: LaunchPlan = serde_json::from_str(
            &node
                .connection
                .query_row::<String, _, _>(
                    "SELECT checkpoint FROM jobs WHERE id=?1",
                    [&a.id],
                    |r| r.get(0),
                )
                .unwrap(),
        )
        .unwrap();
        let session = plan.expected_session.unwrap_or_else(id);
        *runner.session.borrow_mut() = session.clone();
        assert_eq!(
            super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).unwrap()["state"],
            "awaiting_binding"
        );
        assert!(!requests.lock().unwrap().iter().any(|r| r["op"] == "send"));
        let bind = node.pending().unwrap().remove(0).1;
        // A retained invalid outgoing row must not poison receipt lookups or
        // prevent native bootstrap for another, valid binding event.
        node.connection.execute("INSERT INTO events(id,request_hash,wire,state) VALUES ('damaged-record','damaged','not-json','invalid')",[]).unwrap();
        f.store.receive(&bind).unwrap();
        let receipt = f
            .store
            .pending(&f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "receipt")
            .unwrap();
        node.receive(&f.store.prepare_wire(&receipt.id, &owner).unwrap())
            .unwrap();
        let boot =
            super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).unwrap();
        assert_eq!(boot["state"], "bootstrapped");
        assert_eq!(boot["model_ready"], false);
        assert_eq!(
            super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).unwrap(),
            boot
        );
        let messages = requests.lock().unwrap();
        let send = messages.iter().find(|r| r["op"] == "send").unwrap();
        assert!(send["recipientSession"].as_str().is_some());
        assert_eq!(send["recipientEpoch"], 1);
        assert!(send["body"].as_str().unwrap().contains(&session));
        assert!(!send["body"].as_str().unwrap().contains(&worker));
        assert_eq!(messages.iter().filter(|r| r["op"] == "send").count(), 1);
        drop(messages);
        let before: Attempt = document(&f.store.connection, "attempts", &a.id).unwrap();
        assert_eq!(before.state, "awaiting_ready");
        call(
            &mut f.store,
            &worker,
            Mutation::WorkerReady {
                attempt: a.id.clone(),
                epoch: 1,
                session,
            },
        );
        assert_eq!(
            document::<Attempt>(&f.store.connection, "attempts", &a.id)
                .unwrap()
                .state,
            "running"
        );
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        server.join().unwrap();
        std::fs::write(format!("{}.release", capture.display()), b"done").unwrap();
        assert!(launched.wait().success());
        assert!(
            super::runtime::advance(&mut node, &config, &a.id, &f.node_key, &runner).is_err(),
            "an exited runtime cannot receive a stale bootstrap"
        );
    }
}

#[test]
fn runtime_profiles_cannot_override_permissions_identity_or_checkout() {
    for runtime in ["codex", "claude", "pi"] {
        for args in [
            vec!["--yolo"],
            vec!["--sandbox", "danger-full-access"],
            vec!["-c", "sandbox_mode='danger-full-access'"],
            vec!["--session-id", "123"],
            vec!["--resume", "latest"],
            vec!["--cd", "/"],
            vec!["--profile", "unsafe"],
            vec!["--mcp-config", "{}"],
            vec!["--model", "--yolo"],
            vec!["--model"],
        ] {
            assert!(
                super::runtime::validate_args(
                    runtime,
                    &args.into_iter().map(str::to_string).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        assert!(
            super::runtime::validate_args(runtime, &["--model".into(), "allowed-model".into()])
                .is_ok()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_socket_and_mcp_expose_only_scoped_assignment() {
    use super::worker::{self, WorkerRequest};
    let mut f = Fixture::new();
    let task = f.task();
    let (a, token) = f.dispatch(&task);
    let mut node = node_store(&f);
    f.node_key.save_new(&node.dir.join("node.key")).unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
        .unwrap();
    let access = worker::Access {
        state: node.dir.clone(),
        node: "local".into(),
        owner: owner.did(),
        grant: node.grant.clone(),
    };
    let state = node.dir.clone();
    let path = state.clone();
    let (shutdown, stop) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(worker::serve(access, async {
        let _ = stop.await;
    }));
    for _ in 0..100 {
        if path.join("worker.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(path.join("worker.sock").exists());
    tokio::task::spawn_blocking(move||{
        let ctx=worker::call(&state,&WorkerRequest::Context{token:token.clone(),attempt:a.id.clone(),epoch:a.epoch}).unwrap();
        assert_eq!(ctx["attempt"]["id"],a.id);assert!(!ctx.to_string().contains(&token));
        assert!(worker::call(&state,&WorkerRequest::Context{token:"x".repeat(40),attempt:a.id.clone(),epoch:a.epoch}).is_err());
        let requests=format!("{}\n{}\n{}\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"worker_context","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"worker_apply","arguments":{"operation_id":id(),"command":{"method":"pause","params":{"run":a.run,"expected_revision":1}}}}}));
        let mut output=Vec::new();worker::mcp(&state,&token,&a.id,a.epoch,requests.as_bytes(),&mut output).unwrap();
        let output=String::from_utf8(output).unwrap();let responses:Vec<Value>=output.lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(responses.len(),3);assert_eq!(responses[1]["result"]["isError"],false);assert_eq!(responses[2]["result"]["isError"],true);assert!(!output.contains(&token));
    }).await.unwrap();
    shutdown.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!path.join("worker.sock").exists());
}

#[path = "coordinator_tests.rs"]
mod coordinator_tests;
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
#[path = "managed_tests.rs"]
mod managed_tests;
#[path = "verification_tests.rs"]
mod verification_tests;

#[path = "coordinator_launch_tests.rs"]
mod coordinator_launch_tests;

#[path = "live_memory_tests.rs"]
mod live_memory_tests;

#[path = "broker_policy_tests.rs"]
mod broker_policy_tests;

#[path = "delivery_tests.rs"]
mod delivery_tests;

#[path = "reception_tests.rs"]
mod reception_tests;

#[path = "artifact_pipeline_tests.rs"]
mod artifact_pipeline_tests;
#[path = "artifacts_tests.rs"]
mod artifacts_tests;

#[path = "task_amend_tests.rs"]
mod task_amend_tests;

#[path = "run_profiles_tests.rs"]
mod run_profiles_tests;
