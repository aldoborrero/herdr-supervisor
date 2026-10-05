use super::*;
use crate::control::{
    broker_policy::Endpoint,
    coordinator, coordinator_launch, executor, git_pipeline, runtime,
    service_setup::{self, BrokerSpec, Spec},
    transport::{Broker, BrokerConfig},
};
use std::{
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

struct Setup {
    dir: tempfile::TempDir,
    controller: PathBuf,
    bundle: PathBuf,
    spec: Spec,
}
impl Setup {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let spec = Spec {
            node: "local".into(),
            node_state: root.join("node"),
            max_active: 3,
            grant_expires_at: now() + 3600,
            repositories: BTreeMap::from([("/demo".into(), root.join("repository"))]),
            worktree_root: root.join("worktrees"),
            check_timeout_secs: 60,
            broker: BrokerSpec {
                url: "nats://127.0.0.1:4222".into(),
                admin_credentials: root.join("admin.creds"),
                controller_credentials: root.join("controller.creds"),
                node_credentials: root.join("node.creds"),
                private_network_plaintext: true,
            },
            native: coordinator_launch::Config {
                herdr_binary: root.join("bin/herdr"),
                herdr_socket: root.join("herdr.sock"),
                supervisor_binary: root.join("bin/herdr-supervisor"),
                mailer_state: root.join("mailer"),
                mailer_admin_credential: root.join("mailer-admin.token"),
                codex_binary: root.join("bin/codex"),
                mailer_runtime_binary: root.join("bin/herdr-mailer-runtime"),
                claude_development_channel: false,
            },
            notifier_credential: root.join("notifier.token"),
        };
        Self {
            controller: root.join("controller"),
            bundle: root.join("bundle"),
            dir,
            spec,
        }
    }
    fn prepare(&self) -> anyhow::Result<Value> {
        service_setup::prepare(&self.controller, &self.bundle, &self.spec)
    }
    fn file(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.bundle.join(name)).unwrap()
    }
    fn count(&self, table: &str) -> u64 {
        Store::open(&self.controller)
            .unwrap()
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn local_setup_replays_one_identity_enrollment_and_consumable_private_bundle() {
    let f = Setup::new();
    let first = f.prepare().unwrap();
    let before: BTreeMap<_, _> = std::fs::read_dir(&f.bundle)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    assert_eq!(f.prepare().unwrap(), first);
    for (name, bytes) in before {
        let path = f.bundle.join(name);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(f.count("nodes"), 1);
    assert_eq!(f.count("principals"), 2);
    assert_eq!(f.count("runs"), 0);
    assert_eq!(f.count("outbox"), 0);
    assert_eq!(first["services_started"], false);
    assert_eq!(first["models_started"], false);
    assert_eq!(first["broker_provisioned_this_invocation"], false);
    assert!(
        !first
            .to_string()
            .contains(std::str::from_utf8(&f.file("node.token")).unwrap())
    );
    assert!(
        !first
            .to_string()
            .contains(&std::fs::read_to_string(f.controller.join("admin.token")).unwrap())
    );
    assert_eq!(
        executor::read_config(&f.bundle.join("executor.json"))
            .unwrap()
            .herdr_socket,
        f.spec.native.herdr_socket
    );
    assert_eq!(
        git_pipeline::read_config(&f.bundle.join("repositories.json"))
            .unwrap()
            .repositories["/demo"]
            .path,
        f.spec.repositories["/demo"]
    );
    assert_eq!(
        runtime::read_config(&f.bundle.join("runtime.json"))
            .unwrap()
            .node_credential,
        f.bundle.join("node.token")
    );
    assert_eq!(
        coordinator::read_config(&f.bundle.join("notifier.json"))
            .unwrap()
            .sender_credential,
        f.spec.notifier_credential
    );
    assert_eq!(
        coordinator_launch::read_config(&f.bundle.join("launch.json")).unwrap(),
        f.spec.native
    );
    let node = rusqlite::Connection::open(f.spec.node_state.join("node.sqlite")).unwrap();
    assert_eq!(
        node.query_row("SELECT count(*) FROM jobs", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert!(!f.spec.node_state.join("owner.key").exists());
    assert!(!f.controller.join("control.sock").exists());
}

#[test]
fn local_setup_recovers_enrollment_commit_before_failed_token_publication() {
    let f = Setup::new();
    let store = Store::initialize(&f.controller).unwrap();
    store.connection.execute_batch("CREATE TRIGGER deny_enroll BEFORE INSERT ON nodes BEGIN SELECT RAISE(ABORT,'fixture failure'); END;").unwrap();
    assert!(f.prepare().is_err());
    assert_eq!(f.count("nodes"), 0);
    assert_eq!(f.count("principals"), 1);
    let key = std::fs::read(f.spec.node_state.join("node.key")).unwrap();
    let intent = f.file("intent.json");
    secret_file(
        &f.bundle.join("node.token"),
        b"operator-owned existing file",
    )
    .unwrap();
    store
        .connection
        .execute_batch("DROP TRIGGER deny_enroll")
        .unwrap();
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("existing file preserved")
    );
    assert_eq!(f.count("nodes"), 1);
    assert_eq!(f.count("principals"), 2);
    assert_eq!(f.file("node.token"), b"operator-owned existing file");
    std::fs::remove_file(f.bundle.join("node.token")).unwrap();
    f.prepare().unwrap();
    assert_eq!(f.file("intent.json"), intent);
    assert_eq!(
        std::fs::read(f.spec.node_state.join("node.key")).unwrap(),
        key
    );
    assert_eq!(f.count("nodes"), 1);
    assert_eq!(f.count("principals"), 2);
    let grant = f.file("node-grant.json");
    let token = f.file("node.token");
    std::fs::remove_file(f.bundle.join("executor.json")).unwrap();
    std::fs::remove_file(f.bundle.join("node-grant.json")).unwrap();
    f.prepare().unwrap();
    assert_eq!(f.file("node-grant.json"), grant);
    assert_eq!(f.file("node.token"), token);
}

#[test]
fn local_setup_refuses_changed_inputs_files_credentials_and_retired_node() {
    let mut f = Setup::new();
    f.prepare().unwrap();
    f.spec.max_active += 1;
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("inputs changed")
    );
    f.spec.max_active -= 1;
    let original = f.file("runtime.json");
    std::fs::write(f.bundle.join("runtime.json"), b"edited by operator").unwrap();
    assert!(f.prepare().is_err());
    assert_eq!(f.file("runtime.json"), b"edited by operator");
    std::fs::write(f.bundle.join("runtime.json"), &original).unwrap();
    let store = Store::open(&f.controller).unwrap();
    let old = std::fs::read(f.controller.join("admin.token")).unwrap();
    let replacement = "1234567890123456789012345678901234567890";
    store
        .connection
        .execute(
            "UPDATE principals SET token_hash=?1 WHERE role='owner'",
            [digest(replacement.as_bytes())],
        )
        .unwrap();
    std::fs::write(f.controller.join("admin.token"), replacement).unwrap();
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("authority.json")
    );
    store
        .connection
        .execute(
            "UPDATE principals SET token_hash=?1 WHERE role='owner'",
            [digest(&old)],
        )
        .unwrap();
    std::fs::write(f.controller.join("admin.token"), old).unwrap();
    store
        .connection
        .execute("UPDATE nodes SET active=0", [])
        .unwrap();
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("enrollment changed")
    );
    assert_eq!(f.count("nodes"), 1);
}

#[test]
fn local_setup_rejects_aliases_shared_broker_credentials_foreign_bundle_and_symlinks() {
    let mut f = Setup::new();
    let original = f.spec.node_state.clone();
    symlink(f.dir.path(), f.dir.path().join("alias")).unwrap();
    f.spec.node_state = f.dir.path().join("alias/controller");
    assert!(f.prepare().is_err());
    assert!(!f.controller.exists());
    f.spec.node_state = original;
    f.spec.broker.node_credentials = f.spec.broker.admin_credentials.clone();
    assert!(f.prepare().is_err());
    assert!(!f.bundle.exists());
    f.spec.broker.node_credentials = f.dir.path().join("node.creds");
    private_dir(&f.bundle).unwrap();
    secret_file(&f.bundle.join("foreign"), b"preserve").unwrap();
    assert!(f.prepare().is_err());
    assert!(!f.controller.exists());
    std::fs::remove_file(f.bundle.join("foreign")).unwrap();
    f.prepare().unwrap();
    let before = f.file("runtime.json");
    std::fs::remove_file(f.bundle.join("runtime.json")).unwrap();
    let target = f.dir.path().join("operator-runtime");
    std::fs::write(&target, &before).unwrap();
    symlink(&target, f.bundle.join("runtime.json")).unwrap();
    assert!(f.prepare().is_err());
    assert_eq!(std::fs::read(target).unwrap(), before);
    let held = crate::control::api::lock(&f.bundle, "setup.lock").unwrap();
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("another process")
    );
    drop(held);
}

#[test]
fn local_setup_bounded_specs_and_expired_grants_make_no_authority() {
    let mut f = Setup::new();
    let path = f.dir.path().join("spec.json");
    std::fs::write(&path, serde_json::to_vec(&f.spec).unwrap()).unwrap();
    let link = f.dir.path().join("linked-spec.json");
    symlink(&path, &link).unwrap();
    assert!(service_setup::read_spec(&link).is_ok());
    std::fs::write(&path, vec![b' '; 262145]).unwrap();
    assert!(service_setup::read_spec(&path).is_err());
    f.spec.grant_expires_at = now() - 1;
    assert!(f.prepare().is_err());
    assert!(!f.controller.exists());
    assert!(!f.bundle.exists());
}

#[test]
fn local_setup_does_not_adopt_a_replaced_validly_signed_grant() {
    let f = Setup::new();
    f.prepare().unwrap();
    let bytes = f.file("node-grant.json");
    let original: SignedRecord = serde_json::from_slice(&bytes).unwrap();
    let owner = Identity::load(&f.controller.join("owner.key")).unwrap();
    for epoch in [1, 2] {
        let mut record = original.record.clone();
        record.epoch = epoch;
        record.id = id();
        let replacement = owner.sign(record).unwrap();
        replacement.verify(now()).unwrap();
        std::fs::write(
            f.bundle.join("node-grant.json"),
            serde_json::to_vec_pretty(&replacement).unwrap(),
        )
        .unwrap();
        assert!(
            f.prepare()
                .unwrap_err()
                .to_string()
                .contains("node-grant.json")
        );
    }
    std::fs::write(f.bundle.join("node-grant.json"), bytes).unwrap();
    f.prepare().unwrap();
}

#[test]
fn local_setup_refuses_credential_role_collisions_before_creating_state() {
    let mut f = Setup::new();
    let notifier = f.spec.notifier_credential.clone();
    f.spec.notifier_credential = f.spec.native.mailer_admin_credential.clone();
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("distinct credential paths")
    );
    f.spec.notifier_credential = notifier;
    f.spec.broker.node_credentials = f.bundle.join("node.token");
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("generated setup")
    );
    f.spec.broker.node_credentials = f.controller.join("admin.token");
    assert!(
        f.prepare()
            .unwrap_err()
            .to_string()
            .contains("authority files")
    );
    assert!(!f.controller.exists());
    assert!(!f.bundle.exists());
    assert!(!f.spec.node_state.exists());
}

#[tokio::test]
async fn local_setup_provisions_real_broker_only_explicitly_and_retains_bundle_on_failure() {
    let mut f = Setup::new();
    let owner = Store::initialize(&f.controller).unwrap().owner().unwrap();
    let server = NatsServer::start(&owner);
    let admin = server.config("admin");
    let controller = server.config("controller");
    let node = server.config("node");
    f.spec.broker = BrokerSpec {
        url: admin.url.clone(),
        admin_credentials: admin.credentials.clone(),
        controller_credentials: controller.credentials.clone(),
        node_credentials: node.credentials.clone(),
        private_network_plaintext: true,
    };
    let mut result = f.prepare().unwrap();
    let intent = f.file("intent.json");
    let token = f.file("node.token");
    let control = Broker::connect(&controller, &owner, Endpoint::Controller)
        .await
        .unwrap();
    assert!(control.consumer("controller").await.is_err());
    let mut wrong = f.spec.clone();
    wrong.broker.admin_credentials = wrong.broker.node_credentials.clone();
    assert!(
        service_setup::provision(&f.bundle, &wrong, &mut result)
            .await
            .is_err()
    );
    assert_eq!(result["broker_provisioned_this_invocation"], false);
    assert_eq!(f.file("intent.json"), intent);
    service_setup::provision(&f.bundle, &f.spec, &mut result)
        .await
        .unwrap();
    assert_eq!(result["broker_provisioned_this_invocation"], true);
    service_setup::provision(&f.bundle, &f.spec, &mut result)
        .await
        .unwrap();
    assert!(control.consumer("controller").await.is_ok());
    let node = Broker::connect(&node, &owner, Endpoint::Node("local"))
        .await
        .unwrap();
    assert!(node.consumer("local").await.is_ok());
    assert_eq!(
        f.prepare().unwrap()["broker_provisioned_this_invocation"],
        false
    );
    assert_eq!(f.file("node.token"), token);
    let config: BrokerConfig = serde_json::from_slice(&f.file("node-broker.json")).unwrap();
    assert_eq!(config.credentials, f.spec.broker.node_credentials);
}
