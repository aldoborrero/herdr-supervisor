//! Local onboarding composes existing authorities; it never starts a model or
//! installs services. A frozen intent survives interrupted file publication.
use super::{
    api, broker_policy, coordinator, coordinator_launch, executor, git_pipeline,
    identity::{Identity, Record},
    model::{Mutation, Request, digest, id},
    node::NodeStore,
    runtime,
    store::{Store, authenticate, now, private_dir, secret_file},
    transport::{Broker, BrokerConfig},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

const MAX_SPEC: usize = 262144;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerSpec {
    pub url: String,
    pub admin_credentials: PathBuf,
    pub controller_credentials: PathBuf,
    pub node_credentials: PathBuf,
    #[serde(default)]
    pub private_network_plaintext: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub node: String,
    pub node_state: PathBuf,
    pub max_active: u32,
    pub grant_expires_at: i64,
    pub repositories: BTreeMap<String, PathBuf>,
    pub worktree_root: PathBuf,
    pub check_timeout_secs: u64,
    pub broker: BrokerSpec,
    pub native: coordinator_launch::Config,
    pub notifier_credential: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    controller: PathBuf,
    bundle: PathBuf,
    operation: String,
    created_at: i64,
    spec: Spec,
}

pub fn read_spec(path: &Path) -> Result<Spec> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "setup specification must be regular"
    );
    let mut bytes = Vec::new();
    file.take((MAX_SPEC + 1) as u64).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_SPEC, "setup specification too large");
    Ok(serde_json::from_slice(&bytes)?)
}
/// Resolve existing ancestors as well, so two not-yet-created state paths cannot
/// alias through a symlink. Never reinterpret relative paths on a later retry.
fn resolved(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "absolute paths without dot components required"
    );
    let mut parent = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::symlink_metadata(parent) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(parent.file_name().context("setup path has no parent")?);
                parent = parent.parent().context("setup path has no parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut result = std::fs::canonicalize(parent)?;
    for name in suffix.into_iter().rev() {
        result.push(name);
    }
    Ok(result)
}
fn validate(controller: &Path, bundle: &Path, spec: &Spec) -> Result<()> {
    ensure!(
        serde_json::to_vec(spec)?.len() <= MAX_SPEC,
        "setup specification too large"
    );
    broker_policy::node_id(&spec.node)?;
    ensure!(
        spec.max_active > 0 && spec.max_active <= 128,
        "invalid node capacity"
    );
    ensure!(
        spec.grant_expires_at > now(),
        "setup grant expired; no implicit renewal"
    );
    ensure!(
        (1..=3600).contains(&spec.check_timeout_secs),
        "check timeout must be 1..3600 seconds"
    );
    ensure!(
        !spec.repositories.is_empty() && spec.repositories.len() <= 128,
        "setup needs 1..128 repository mappings"
    );
    spec.native.validate()?;
    let roots = [
        controller,
        &spec.node_state,
        bundle,
        &spec.native.mailer_state,
        &spec.worktree_root,
    ]
    .map(resolved);
    let roots = roots.into_iter().collect::<Result<Vec<_>>>()?;
    for (i, path) in roots.iter().enumerate() {
        for other in roots.iter().skip(i + 1) {
            ensure!(
                !path.starts_with(other) && !other.starts_with(path),
                "controller, node, bundle, mailer and worktrees must be separate directories"
            );
        }
    }
    for (logical, path) in &spec.repositories {
        ensure!(
            Path::new(logical).is_absolute(),
            "absolute logical repository required"
        );
        let path = resolved(path)?;
        for state in roots.iter().take(3) {
            ensure!(
                !path.starts_with(state) && !state.starts_with(&path),
                "service state must be outside repository mappings"
            );
        }
    }
    let creds = [
        &spec.broker.admin_credentials,
        &spec.broker.controller_credentials,
        &spec.broker.node_credentials,
    ]
    .map(|path| resolved(path.as_path()));
    let creds = creds.into_iter().collect::<Result<Vec<_>>>()?;
    ensure!(
        creds[0] != creds[1] && creds[0] != creds[2] && creds[1] != creds[2],
        "broker administrator, controller and node need distinct credential paths"
    );
    let mut external = creds;
    external.push(resolved(&spec.notifier_credential)?);
    external.push(resolved(&spec.native.mailer_admin_credential)?);
    let reserved = [
        controller.join("owner.key"),
        controller.join("admin.token"),
        spec.node_state.join("node.key"),
    ]
    .map(|path| resolved(&path));
    let reserved = reserved.into_iter().collect::<Result<Vec<_>>>()?;
    for (i, path) in external.iter().enumerate() {
        ensure!(
            !path.starts_with(&roots[2]) && !reserved.contains(path),
            "external credentials cannot use generated setup or authority files"
        );
        ensure!(
            !external.iter().skip(i + 1).any(|other| other == path),
            "broker and mailer roles need distinct credential paths"
        );
    }
    ensure!(
        spec.broker.url.len() <= 2048
            && !spec.broker.url.contains('@')
            && !spec.broker.url.chars().any(char::is_whitespace),
        "broker URL must not embed credentials or whitespace"
    );
    let _: async_nats::ServerAddr = spec.broker.url.parse()?;
    Ok(())
}
fn read_private(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && meta.permissions().mode() & 0o077 == 0
            && meta.len() <= MAX_SPEC as u64 + 65536,
        "setup record must be private, regular and bounded"
    );
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_SPEC + 65537) as u64)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= MAX_SPEC + 65536, "setup record too large");
    Ok(data)
}
fn durable_dir(path: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = path;
    while std::fs::symlink_metadata(current)
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        missing.push(current.to_owned());
        current = current.parent().context("state parent")?;
    }
    private_dir(path)?;
    for created in missing {
        std::fs::File::open(&created)?.sync_all()?;
        std::fs::File::open(created.parent().context("state parent")?)?.sync_all()?;
    }
    Ok(())
}
/// Durable no-replace publication. Existing edits or symlinks never get replaced.
fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::symlink_metadata(path).is_ok() {
        ensure!(
            read_private(path)? == bytes,
            "setup output differs; existing file preserved: {}",
            path.display()
        );
        std::fs::File::open(path)?.sync_all()?;
        std::fs::File::open(path.parent().context("setup output parent")?)?.sync_all()?;
        return Ok(());
    }
    let parent = path.parent().context("setup output parent")?;
    let tmp = parent.join(format!(".pending-{}", id()));
    secret_file(&tmp, bytes)?;
    let linked = std::fs::hard_link(&tmp, path);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
            read_private(path)? == bytes,
            "setup output changed during publication"
        ),
        Err(error) => return Err(error.into()),
    }
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn document(path: &Path, value: &impl Serialize) -> Result<()> {
    publish(path, &serde_json::to_vec_pretty(value)?)
}
fn broker(spec: &Spec, credentials: &Path) -> BrokerConfig {
    BrokerConfig {
        url: spec.broker.url.clone(),
        credentials: credentials.to_owned(),
        private_network_plaintext: spec.broker.private_network_plaintext,
    }
}

pub fn prepare(controller: &Path, bundle: &Path, spec: &Spec) -> Result<Value> {
    validate(controller, bundle, spec)?;
    let controller = resolved(controller)?;
    let bundle = resolved(bundle)?;
    durable_dir(&bundle)?;
    let _lock = api::lock(&bundle, "setup.lock")?;
    let path = bundle.join("intent.json");
    let intent: Intent = if path.exists() {
        serde_json::from_slice(&read_private(&path)?)?
    } else {
        for entry in std::fs::read_dir(&bundle)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            ensure!(
                name == "setup.lock"
                    || name
                        .strip_prefix(".pending-")
                        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok()),
                "setup bundle is not empty and has no owned intent"
            );
        }
        let intent = Intent {
            version: 1,
            controller: controller.clone(),
            bundle: bundle.clone(),
            operation: id(),
            created_at: now(),
            spec: spec.clone(),
        };
        document(&path, &intent)?;
        intent
    };
    ensure!(
        intent.version == 1
            && intent.controller == controller
            && intent.bundle == bundle
            && serde_json::to_value(&intent.spec)? == serde_json::to_value(spec)?,
        "setup inputs changed; resume the original specification and bundle"
    );
    durable_dir(&controller)?;
    let mut store = Store::initialize(&controller)?;
    durable_dir(&spec.node_state)?;
    let _node_lock = api::lock(&spec.node_state, "setup-node.lock")?;
    let key_path = spec.node_state.join("node.key");
    let node_key = if key_path.exists() {
        Identity::load(&key_path)?
    } else {
        let key = Identity::generate()?;
        key.save_new(&key_path)?;
        key
    };
    let owner = store.owner()?;
    let token = runtime::credential(&controller.join("admin.token"))?;
    document(
        &bundle.join("authority.json"),
        &json!({"owner":owner,"node":node_key.did(),"owner_credential_hash":digest(token.as_bytes())}),
    )?;
    let enrolled = store.apply(&Request {
        token,
        operation_id: intent.operation.clone(),
        command: Mutation::EnrollNode {
            id: spec.node.clone(),
            key: node_key.did(),
            max_active: spec.max_active,
        },
    })?;
    let node_token = enrolled["credential"]
        .as_str()
        .context("missing enrolled node credential")?;
    let principal = authenticate(&store.connection, node_token)?;
    ensure!(
        principal.role == "node"
            && principal.node.as_deref() == Some(&spec.node)
            && principal.generation == 1,
        "setup node authority changed"
    );
    let (key, epoch, active): (String, u64, bool) = store.connection.query_row(
        "SELECT key,epoch,active FROM nodes WHERE id=?1",
        [&spec.node],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        key == node_key.did() && epoch == 1 && active,
        "setup node enrollment changed"
    );
    publish(&bundle.join("node.token"), node_token.as_bytes())?;
    let grant_path = bundle.join("node-grant.json");
    // Derive the same signed bytes from the frozen operation/time on every
    // retry. A missing export can be recovered; a replaced grant is refused.
    let grant = Identity::load(&controller.join("owner.key"))?.sign(Record {
        record_type: "local.herdr.supervisor.nodeGrant".into(),
        id: intent.operation,
        owner: owner.clone(),
        author: owner.clone(),
        recipient: node_key.did(),
        epoch: 1,
        created_at: intent.created_at,
        expires_at: spec.grant_expires_at,
        payload: json!({"node":spec.node,"key":node_key.did()}),
    })?;
    grant.verify(now())?;
    document(&grant_path, &grant)?;
    let _node = NodeStore::open(&spec.node_state, &spec.node, &owner, grant, &node_key)?;
    let verifier = git_pipeline::Config {
        repositories: spec
            .repositories
            .iter()
            .map(|(name, path)| {
                (
                    name.clone(),
                    git_pipeline::Repository {
                        path: path.clone(),
                        node_remotes: BTreeMap::new(),
                    },
                )
            })
            .collect(),
        check_timeout_secs: spec.check_timeout_secs,
    };
    let executor = executor::ExecutorConfig {
        repositories: spec.repositories.clone(),
        base_remotes: BTreeMap::new(),
        worktree_root: spec.worktree_root.clone(),
        herdr_binary: spec.native.herdr_binary.to_string_lossy().into_owned(),
        herdr_socket: spec.native.herdr_socket.clone(),
    };
    let runtime = runtime::RuntimeConfig {
        supervisor_binary: spec.native.supervisor_binary.clone(),
        mailer_state: spec.native.mailer_state.clone(),
        mailer_admin_credential: spec.native.mailer_admin_credential.clone(),
        node_credential: bundle.join("node.token"),
        codex_binary: spec.native.codex_binary.clone(),
        mailer_runtime_binary: spec.native.mailer_runtime_binary.clone(),
        claude_development_channel: spec.native.claude_development_channel,
    };
    let notifier = coordinator::Config {
        mailer_state: spec.native.mailer_state.clone(),
        sender_credential: spec.notifier_credential.clone(),
        supervisor_binary: spec.native.supervisor_binary.clone(),
    };
    for (name, value) in [
        (
            "admin-broker.json",
            serde_json::to_value(broker(spec, &spec.broker.admin_credentials))?,
        ),
        (
            "controller-broker.json",
            serde_json::to_value(broker(spec, &spec.broker.controller_credentials))?,
        ),
        (
            "node-broker.json",
            serde_json::to_value(broker(spec, &spec.broker.node_credentials))?,
        ),
        ("repositories.json", serde_json::to_value(verifier)?),
        ("executor.json", serde_json::to_value(executor)?),
        ("runtime.json", serde_json::to_value(runtime)?),
        ("notifier.json", serde_json::to_value(notifier)?),
        ("launch.json", serde_json::to_value(&spec.native)?),
        (
            "broker-policy.json",
            broker_policy::policy(&owner, std::slice::from_ref(&spec.node))?,
        ),
    ] {
        document(&bundle.join(name), &value)?;
    }
    let bin = &spec.native.supervisor_binary;
    let commands = json!({
        "controller":[bin,"control","--state",controller,"run-controller","--broker",bundle.join("controller-broker.json"),"--repositories",bundle.join("repositories.json"),"--coordinator-config",bundle.join("notifier.json")],
        "node":[bin,"control","--state",spec.node_state,"run-node","--broker",bundle.join("node-broker.json"),"--executor",bundle.join("executor.json"),"--runtime-config",bundle.join("runtime.json"),"--node",spec.node,"--owner",owner,"--grant",grant_path],
        "provision":[bin,"control","--state",controller,"provision","--broker",bundle.join("admin-broker.json"),"--target",spec.node]
    });
    document(&bundle.join("commands.json"), &commands)?;
    Ok(
        json!({"owner":owner,"node":spec.node,"node_key":node_key.did(),"bundle":bundle,"grant_expires_at":spec.grant_expires_at,"launch_config":bundle.join("launch.json"),"commands":commands,"services_started":false,"models_started":false,"broker_provisioned_this_invocation":false,"runtime_health_checked":false}),
    )
}
pub async fn provision(bundle: &Path, spec: &Spec, result: &mut Value) -> Result<()> {
    let owner = result["owner"].as_str().context("missing prepared owner")?;
    let _lock = api::lock(bundle, "setup.lock")?;
    let broker = Broker::connect(
        &broker(spec, &spec.broker.admin_credentials),
        owner,
        broker_policy::Endpoint::Provisioner,
    )
    .await?;
    broker
        .provision(&["controller".into(), spec.node.clone()])
        .await?;
    result["broker_provisioned_this_invocation"] = json!(true);
    Ok(())
}
