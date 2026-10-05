//! Read-only observations, never repair, registration, publication or a model
//! readiness receipt. External errors and payloads do not enter diagnostics.
use super::*;
use crate::{
    control::{herdr_events, identity::SignedRecord, store::own},
    runner::{Cmd, RealRunner, Runner},
};
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, Instant};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(10);

fn row(check: &str, passed: bool, action: &str) -> Value {
    json!({"check":check,"status":if passed{"passed"}else{"failed"},"action":if passed{""}else{action}})
}
fn report(checks: Vec<Value>) -> Value {
    json!({"checks_passed":checks.iter().all(|c|c["status"]=="passed"),"checks":checks,"observed_at":now(),"services_started":false,"models_started":false,"model_readiness":"not_checked","not_checked":["model_authentication","native_cli_compatibility","mailer_admin_authorization","broker_acl_isolation","repository_write_access","worker_readiness"],"consistency":"independent observations; revalidated local authority; not an atomic health guarantee"})
}

fn db(path: &Path) -> Result<Connection> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "private regular database required"
    );
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    connection.execute_batch("PRAGMA query_only=ON; BEGIN;")?;
    Ok(connection)
}
fn same(path: &Path, value: &impl Serialize) -> Result<()> {
    ensure!(
        read_private(path)? == serde_json::to_vec_pretty(value)?,
        "bundle file differs"
    );
    Ok(())
}
fn private_state(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.permissions().mode() & 0o077 == 0,
        "private real state directory required"
    );
    Ok(())
}
fn load(controller: &Path, bundle: &Path) -> Result<(Intent, String)> {
    let controller = resolved(controller)?;
    let bundle = resolved(bundle)?;
    private_state(&controller)?;
    private_state(&bundle)?;
    let intent: Intent = serde_json::from_slice(&read_private(&bundle.join("intent.json"))?)?;
    ensure!(
        intent.version == 1 && intent.controller == controller && intent.bundle == bundle,
        "bundle scope mismatch"
    );
    let spec = &intent.spec;
    validate(&controller, &bundle, spec)?;
    private_state(&spec.node_state)?;
    let c = db(&controller.join("control.sqlite"))?;
    let n = db(&spec.node_state.join("node.sqlite"))?;
    ensure!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))? == 9
            && n.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))? == 8,
        "database needs a supported service migration before checking"
    );
    let owner: String =
        c.query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?;
    let owner_key = Identity::load(&controller.join("owner.key"))?;
    let node = Identity::load(&spec.node_state.join("node.key"))?.did();
    ensure!(owner_key.did() == owner, "owner mismatch");
    let token = runtime::credential(&controller.join("admin.token"))?;
    let principal = authenticate(&c, &token)?;
    own(&principal)?;
    same(
        &bundle.join("authority.json"),
        &json!({"owner":owner,"node":node,"owner_credential_hash":digest(token.as_bytes())}),
    )?;
    let command = Mutation::EnrollNode {
        id: spec.node.clone(),
        key: node.clone(),
        max_active: spec.max_active,
    };
    let (hash, response): (String, String) = c.query_row(
        "SELECT request_hash,response FROM operations WHERE principal=?1 AND id=?2",
        rusqlite::params![principal.id, intent.operation],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        hash == digest(&serde_json::to_vec(&command)?),
        "enrollment operation differs"
    );
    let response: Value = serde_json::from_str(&response)?;
    let node_token = runtime::credential(&bundle.join("node.token"))?;
    ensure!(
        response["id"] == spec.node
            && response["epoch"] == 1
            && response["credential"] == node_token,
        "enrollment receipt differs"
    );
    let principal = authenticate(&c, &node_token)?;
    ensure!(
        principal.role == "node"
            && principal.node.as_deref() == Some(&spec.node)
            && principal.generation == 1,
        "node credential changed"
    );
    let enrollment: (String, u64, u32, bool) = c.query_row(
        "SELECT key,epoch,max_active,active FROM nodes WHERE id=?1",
        [&spec.node],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        enrollment == (node.clone(), 1, spec.max_active, true),
        "node enrollment changed"
    );
    let authority: (String, String, String, u64) = n.query_row(
        "SELECT owner,node,key,epoch FROM authority WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        authority == (owner.clone(), spec.node.clone(), node.clone(), 1),
        "node database authority changed"
    );
    let grant: SignedRecord =
        serde_json::from_slice(&read_private(&bundle.join("node-grant.json"))?)?;
    grant.verify(now())?;
    ensure!(
        serde_json::to_value(&grant.record)?
            == serde_json::to_value(grant_record(&intent, &owner, &node))?,
        "grant record differs"
    );
    same(&bundle.join("node-grant.json"), &grant)?;
    for (name, value) in manifest(&controller, &bundle, spec, &owner)? {
        same(&bundle.join(name), &value)?;
    }
    Ok((intent, owner))
}

async fn mailer(socket: &Path, request: Value) -> Result<Value> {
    timeout(DEADLINE, async {
        let mut stream = UnixStream::connect(socket).await?;
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let mut reader = BufReader::new(stream).take(65537);
        let mut bytes = Vec::new();
        reader.read_until(b'\n', &mut bytes).await?;
        ensure!(
            bytes.len() <= 65536 && bytes.last() == Some(&b'\n'),
            "invalid bounded mailer response"
        );
        let response: Value = serde_json::from_slice(&bytes)?;
        ensure!(response.get("error").is_none(), "mailer request rejected");
        response.get("data").cloned().context("missing mailer data")
    })
    .await?
}
async fn sender(spec: &Spec) -> Result<()> {
    let token = runtime::credential(&spec.notifier_credential)?;
    let socket = spec.native.mailer_state.join("daemon.sock");
    let caps = mailer(&socket, json!({"op":"capabilities","token":token})).await?;
    ensure!(
        caps["protocol"] == 2
            && caps["idempotentSend"] == true
            && caps["recipientPrecondition"] == true,
        "mailer protocol 2 required"
    );
    let doctor = mailer(&socket, json!({"op":"doctor","token":token})).await?;
    ensure!(
        doctor["sqlite"] == "ok"
            && doctor["owner"]
                .as_str()
                .is_some_and(|s| crate::control::identity::verifying_key(s).is_ok())
            && doctor["agent"]
                .as_str()
                .is_some_and(|s| crate::control::model::identifier(s).is_ok())
            && doctor["session"]
                .as_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
            && doctor["epoch"].as_u64().is_some_and(|n| n > 0),
        "invalid mailer sender health"
    );
    Ok(())
}
async fn consumer(spec: &Spec, owner: &str, node: bool) -> Result<()> {
    timeout(DEADLINE, async {
        let (config, target, endpoint) = if node {
            (
                broker(spec, &spec.broker.node_credentials),
                spec.node.as_str(),
                broker_policy::Endpoint::Node(&spec.node),
            )
        } else {
            (
                broker(spec, &spec.broker.controller_credentials),
                "controller",
                broker_policy::Endpoint::Controller,
            )
        };
        let connected = Broker::connect(&config, owner, endpoint).await?;
        connected.consumer(target).await?;
        Ok(())
    })
    .await?
}
fn repositories(spec: &Spec) -> Vec<Value> {
    let until = Instant::now() + DEADLINE;
    spec.repositories.iter().map(|(name,path)|{
        let Some(remaining)=until.checked_duration_since(Instant::now()) else {
            let mut check=row("repository",false,"Repository probe time budget exhausted; no command ran for this mapping.");check["repository"]=json!(name);return check;
        };
        let checked=RealRunner.run(&Cmd::new("git",remaining.min(Duration::from_secs(5))).args(["-c","core.hooksPath=/dev/null","rev-parse","--absolute-git-dir"]).cwd(path).env("GIT_OPTIONAL_LOCKS","0").env_remove("GIT_DIR").env_remove("GIT_WORK_TREE"));
        let passed=checked.is_ok_and(|o|o.success()&&!o.stdout.trim().is_empty());
        let mut check=row("repository",passed,"Verify the mapped Git directory and Git executable available to this service user.");check["repository"]=json!(name);check
    }).collect()
}

pub async fn check(controller: &Path, bundle: &Path) -> Result<Value> {
    let loaded = load(controller, bundle);
    let mut checks = vec![row(
        "bundle_integrity",
        loaded.is_ok(),
        "Inspect the frozen bundle, private file permissions, current controller/node authority and grant expiry; check never repairs or renews state.",
    )];
    let Ok((intent, owner)) = loaded else {
        return Ok(report(checks));
    };
    let spec = &intent.spec;
    let admin = runtime::credential(&spec.native.mailer_admin_credential);
    checks.push(row("mailer_admin_file",admin.is_ok_and(|token|!token.is_empty()),"Supply the configured private mailer admin credential file; remote admin authorization is not tested."));
    let (controller_broker, node_broker, herdr, mail) = tokio::join!(
        consumer(spec, &owner, false),
        consumer(spec, &owner, true),
        herdr_events::snapshot_with_id(&spec.native.herdr_socket, "supervisor-check"),
        sender(spec)
    );
    checks.push(row("controller_broker",controller_broker.is_ok(),"Check the controller's broker credentials/TLS and provision its owner stream and consumer."));
    checks.push(row(
        "node_broker",
        node_broker.is_ok(),
        "Check the node's broker credentials/TLS and provision its exact consumer.",
    ));
    checks.push(row(
        "herdr_snapshot",
        herdr.is_ok(),
        "Check the configured Herdr socket and protocol-22-or-later snapshot support.",
    ));
    checks.push(row(
        "mailer_sender",
        mail.is_ok(),
        "Check the notifier session token, mailer SQLite health and protocol-2 send capabilities.",
    ));
    let repos = intent.spec.clone();
    checks.extend(tokio::task::spawn_blocking(move || repositories(&repos)).await?);
    let unchanged = load(controller, bundle).is_ok_and(|(current, current_owner)| {
        current_owner == owner
            && serde_json::to_value(current).ok() == serde_json::to_value(&intent).ok()
    });
    checks.push(row(
        "authority_rechecked",
        unchanged,
        "Bundle or authority changed during checking; inspect and repeat before starting services.",
    ));
    let mut result = report(checks);
    result["owner"] = json!(owner);
    result["node"] = json!(intent.spec.node);
    Ok(result)
}
