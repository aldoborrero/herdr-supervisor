//! Exact-identity runtime launch and native mailer bootstrap. All mutations use
//! durable markers; uncertain registrations/launches require reconciliation.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::executor::CheckoutPlan;
use super::identity::Identity;
use super::model::{Mutation, Request, Resource, id};
use super::node::{ExecuteOrder, NodeStore};
use super::store::{private_dir, secret_file};
use crate::herdr::Herdr;
use crate::remote::quote;
use crate::runner::Runner;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub supervisor_binary: PathBuf,
    pub mailer_state: PathBuf,
    pub mailer_admin_credential: PathBuf,
    pub node_credential: PathBuf,
    pub codex_binary: PathBuf,
    pub mailer_runtime_binary: PathBuf,
    /// Explicit owner opt-in to Claude's development channel mechanism.
    pub claude_development_channel: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchPlan {
    pub checkout: CheckoutPlan,
    pub config: RuntimeConfig,
    pub expected_session: Option<String>,
    pub registration_started: bool,
    pub registration: Option<Value>,
    pub launch_started: bool,
    pub wrapper_started: bool,
    pub wrapper_pid: Option<u32>,
    pub bind_event: Option<String>,
    pub bootstrap: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Invocation {
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
}

pub fn credential(path: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0 && meta.len() <= 16384,
        "credential must be a bounded private regular file"
    );
    let text = std::fs::read_to_string(path)?;
    Ok(text.trim().into())
}

fn files(store: &NodeStore, attempt: &str) -> PathBuf {
    store.dir.join("workers").join(attempt)
}

fn save(store: &NodeStore, attempt: &str, plan: &LaunchPlan) -> Result<()> {
    store.connection.execute(
        "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
        params![attempt, serde_json::to_string(plan)?],
    )?;
    Ok(())
}

pub(super) fn ensure_file(path: &Path, contents: &[u8]) -> Result<()> {
    if path.try_exists()? {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path)?;
        ensure!(
            meta.is_file() && meta.permissions().mode() & 0o077 == 0,
            "runtime file must be private and regular"
        );
        ensure!(
            std::fs::read(path)? == contents,
            "runtime file content changed"
        );
        return Ok(());
    }
    secret_file(path, contents)
}

/// Owner-selected model/tuning flags may not override identity, permissions or roots.
/// An allowlist prevents config-file/profile switches from undoing launcher fencing.
pub fn validate_args(runtime: &str, args: &[String]) -> Result<()> {
    let valued: &[&str] = match runtime {
        "codex" => &["--model", "-m"],
        "claude" => &["--model", "--effort"],
        "pi" => &["--model", "--provider", "--thinking"],
        _ => bail!("unsupported runtime"),
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        ensure!(
            valued.contains(&flag.as_str()),
            "runtime profile flag is not allowed: {flag}"
        );
        let value = it.next().context("runtime profile flag needs a value")?;
        ensure!(
            !value.is_empty()
                && value.len() <= 256
                && !value.starts_with('-')
                && !value.contains(['\n', '\r', '\0']),
            "invalid runtime profile value"
        );
    }
    Ok(())
}

pub fn invocation(state: &Path, order: &ExecuteOrder, plan: &LaunchPlan) -> Result<Invocation> {
    validate_args(&order.profile.runtime, &order.profile.argv)?;
    let dir = state.join("workers").join(&order.attempt.id);
    let environment = BTreeMap::from([
        (
            "HERDR_SUPERVISOR_STATE".into(),
            state.to_string_lossy().into(),
        ),
        ("HERDR_SUPERVISOR_ATTEMPT".into(), order.attempt.id.clone()),
        (
            "HERDR_SUPERVISOR_EPOCH".into(),
            order.attempt.epoch.to_string(),
        ),
        (
            "HERDR_SUPERVISOR_CREDENTIAL".into(),
            dir.join("worker.token").to_string_lossy().into(),
        ),
        (
            "HERDR_MAILER_STATE".into(),
            plan.config.mailer_state.to_string_lossy().into(),
        ),
        (
            "HERDR_MAILER_CREDENTIAL".into(),
            dir.join("mailer.token").to_string_lossy().into(),
        ),
    ]);
    native_invocation(
        &order.profile,
        &Programs {
            codex_binary: plan.config.codex_binary.clone(),
            mailer_runtime_binary: plan.config.mailer_runtime_binary.clone(),
            claude_development_channel: plan.config.claude_development_channel,
        },
        &dir,
        &plan.checkout.worktree,
        environment,
    )
}

pub struct Programs {
    pub codex_binary: PathBuf,
    pub mailer_runtime_binary: PathBuf,
    pub claude_development_channel: bool,
}

pub fn native_invocation(
    profile: &super::model::Profile,
    programs: &Programs,
    dir: &Path,
    cwd: &Path,
    mut environment: BTreeMap<String, String>,
) -> Result<Invocation> {
    validate_args(&profile.runtime, &profile.argv)?;
    let (program, mut argv) = match profile.runtime.as_str() {
        "codex" => (
            programs.codex_binary.clone(),
            vec![
                "--approve-for-me".into(),
                "--sandbox".into(),
                "workspace-write".into(),
                "--cd".into(),
                cwd.to_string_lossy().into(),
            ],
        ),
        "claude" | "pi" => {
            if profile.runtime == "claude" {
                ensure!(
                    programs.claude_development_channel,
                    "Claude development channel requires owner opt-in"
                );
            }
            environment.insert(
                "HERDR_MAILER_CONFIG".into(),
                dir.join("mailer.json").to_string_lossy().into(),
            );
            (
                programs.mailer_runtime_binary.clone(),
                if profile.runtime == "claude" {
                    vec!["--development-channel".into()]
                } else {
                    vec![]
                },
            )
        }
        _ => bail!("unsupported runtime"),
    };
    argv.extend(profile.argv.clone());
    ensure!(program.is_absolute(), "runtime binary must be absolute");
    Ok(Invocation {
        program,
        argv,
        cwd: cwd.to_path_buf(),
        environment,
    })
}

fn registration(store: &NodeStore, order: &ExecuteOrder, plan: &mut LaunchPlan) -> Result<()> {
    let session = plan
        .expected_session
        .clone()
        .context("native session has not been observed")?;
    let dir = files(store, &order.attempt.id);
    let receipt = dir.join("registration.json");
    if plan.registration.is_none() {
        let value: Value = if receipt.try_exists()? {
            serde_json::from_str(&credential(&receipt)?)?
        } else {
            ensure!(
                !plan.registration_started,
                "mailer registration outcome is uncertain; do not rotate a possibly registered session"
            );
            plan.registration_started = true;
            save(store, &order.attempt.id, plan)?;
            let value = super::api::call_json(
                &plan.config.mailer_state.join("daemon.sock"),
                &json!({
                    "op":"register","token":credential(&plan.config.mailer_admin_credential)?,
                    "agent":format!("hs-{}",order.attempt.id),"runtime":order.profile.runtime,"location":session,
                }),
            )?;
            secret_file(&receipt, &serde_json::to_vec(&value)?)?;
            value
        };
        let s = &value["session"];
        ensure!(
            s["location"] == session
                && s["runtime"] == order.profile.runtime
                && s["agent"] == format!("hs-{}", order.attempt.id),
            "mailer returned a different receiver"
        );
        uuid::Uuid::parse_str(s["id"].as_str().context("mailer session ID")?)?;
        ensure!(
            s["epoch"].as_u64().is_some_and(|v| v > 0),
            "mailer epoch missing"
        );
        let token = value["token"].as_str().context("mailer token missing")?;
        ensure!((32..=256).contains(&token.len()), "invalid mailer token");
        ensure_file(&dir.join("mailer.token"), token.as_bytes())?;
        plan.registration = Some(s.clone());
        save(store, &order.attempt.id, plan)?;
    }
    if order.profile.runtime != "codex" {
        ensure_file(
            &dir.join("mailer.json"),
            &serde_json::to_vec(
                &json!({"runtime":order.profile.runtime,"location":session,"state":plan.config.mailer_state,"credential":dir.join("mailer.token"),"receipts":dir.join("native-receipts")}),
            )?,
        )?;
    }
    Ok(())
}

/// Advances one launch, observation and binding cycle. Repeated calls reconcile;
/// they never type a second launcher into a pane after an uncertain first call.
pub fn advance(
    store: &mut NodeStore,
    config: &RuntimeConfig,
    attempt: &str,
    identity: &Identity,
    runner: &dyn Runner,
) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "execution.lock")?;
    store.grant.verify(jiff::Timestamp::now().as_second())?;
    let exited: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_processes WHERE attempt=?1 AND state='exited')",
        [attempt],
        |r| r.get(0),
    )?;
    ensure!(
        !exited,
        "runtime already exited; reconcile its durable receipt"
    );
    let (epoch, state, doc, checkpoint): (u64, String, String, String) =
        store.connection.query_row(
            "SELECT epoch,state,doc,checkpoint FROM jobs WHERE id=?1",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
    ensure!(
        ["prepared", "launching", "awaiting_binding", "bootstrapped"].contains(&state.as_str()),
        "job is not eligible for runtime launch"
    );
    let order: ExecuteOrder = serde_json::from_str(&doc)?;
    ensure!(
        order.attempt.epoch == epoch && order.attempt.node == store.node_id,
        "attempt fenced"
    );
    validate_args(&order.profile.runtime, &order.profile.argv)?;
    for path in [
        &config.supervisor_binary,
        &config.mailer_state,
        &config.mailer_admin_credential,
        &config.node_credential,
    ] {
        ensure!(
            path.is_absolute(),
            "runtime configuration paths must be absolute"
        );
    }
    let mut plan: LaunchPlan = if state == "prepared" {
        let checkout: CheckoutPlan = serde_json::from_str(&checkpoint)?;
        let plan = LaunchPlan {
            checkout,
            config: config.clone(),
            expected_session: if order.profile.runtime == "codex" {
                None
            } else {
                Some(id())
            },
            registration_started: false,
            registration: None,
            launch_started: false,
            wrapper_started: false,
            wrapper_pid: None,
            bind_event: None,
            bootstrap: None,
        };
        invocation(&store.dir, &order, &plan)?;
        store.connection.execute(
            "UPDATE jobs SET state='launching',checkpoint=?2 WHERE id=?1",
            params![attempt, serde_json::to_string(&plan)?],
        )?;
        plan
    } else {
        serde_json::from_str(&checkpoint)?
    };
    ensure!(
        &plan.config == config,
        "runtime configuration changed for an existing attempt"
    );
    let dir = files(store, attempt);
    private_dir(&store.dir.join("workers"))?;
    private_dir(&dir)?;
    ensure_file(&dir.join("worker.token"), order.credential.as_bytes())?;
    let herdr = Herdr::new(&plan.checkout.herdr_binary, &plan.checkout.socket, runner);
    if !plan.launch_started {
        super::executor::inspect_checkout(runner, &plan.checkout)?;
        super::executor::observe_pane(&herdr, &plan.checkout)?;
        if order.profile.runtime != "codex" {
            registration(store, &order, &mut plan)?;
        }
        let binding = plan
            .checkout
            .binding
            .as_ref()
            .context("missing pane binding")?;
        let command = format!(
            "{} control --state {} run-runtime {}",
            quote(config.supervisor_binary.to_str().context("binary UTF8")?),
            quote(store.dir.to_str().context("state UTF8")?),
            quote(attempt)
        );
        plan.launch_started = true;
        save(store, attempt, &plan)?;
        let _outcome = herdr.call(
            &["pane", "run", &binding.pane, &command],
            Duration::from_secs(10),
        );
        // Do not hold the lease while waiting for our wrapper to acquire it.
        return Ok(json!({"state":"launching","model_ready":false}));
    }
    let binding = plan
        .checkout
        .binding
        .clone()
        .context("missing pane binding")?;
    let agents = herdr.agent_list()?;
    let agents: Vec<_> = agents
        .iter()
        .filter(|a| a.pane_id == binding.pane)
        .collect();
    ensure!(
        agents.len() == 1,
        "runtime launch unresolved; no automatic second launch"
    );
    let agent = agents[0];
    ensure!(
        agent.terminal_id == binding.terminal
            && agent.workspace_id == binding.workspace
            && agent.works_in(plan.checkout.worktree.to_str().context("checkout UTF8")?)
            && agent.agent == order.profile.runtime,
        "observed runtime resource identity differs"
    );
    let session = agent
        .agent_session
        .as_ref()
        .context("Herdr has not reported a native session")?
        .value
        .clone();
    uuid::Uuid::parse_str(&session)?;
    if let Some(expected) = &plan.expected_session {
        ensure!(&session == expected, "native UUID mismatch");
    }
    ensure!(
        plan.wrapper_started,
        "runtime not launched by the owned wrapper"
    );
    plan.expected_session = Some(session.clone());
    save(store, attempt, &plan)?;
    registration(store, &order, &mut plan)?;
    let receiver = plan
        .registration
        .clone()
        .context("receiver registration missing")?;
    if plan.bind_event.is_none() {
        let resource = Resource {
            repo_common_dir: plan.checkout.common_dir.to_string_lossy().into(),
            worktree: plan.checkout.worktree.to_string_lossy().into(),
            branch: plan.checkout.branch.clone(),
            workspace: binding.workspace.clone(),
            pane: binding.pane.clone(),
            terminal: binding.terminal.clone(),
            session,
            runtime: order.profile.runtime.clone(),
            mailer_agent: receiver["agent"].as_str().unwrap().into(),
            mailer_epoch: receiver["epoch"].as_u64().unwrap(),
        };
        plan.bind_event = Some(store.enqueue(
            &Request {
                token: credential(&config.node_credential)?,
                operation_id: format!("bind-{attempt}-{epoch}"),
                command: Mutation::BindResource {
                    attempt: attempt.into(),
                    epoch,
                    resource,
                },
            },
            identity,
        )?);
        save(store, attempt, &plan)?;
        store.connection.execute(
            "UPDATE jobs SET state='awaiting_binding' WHERE id=?1",
            [attempt],
        )?;
    }
    let applied:Option<String>=store.connection.query_row("SELECT response FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.id')=?1 AND state='controller_applied'",[plan.bind_event.as_ref().unwrap()],|r|r.get(0)).optional()?;
    if applied.is_none() {
        return Ok(json!({"state":"awaiting_binding","model_ready":false}));
    }
    if plan.bootstrap.is_none() {
        let token = credential(&dir.join("mailer.token"))?;
        let caps = super::api::call_json(
            &config.mailer_state.join("daemon.sock"),
            &json!({"op":"capabilities","token":token}),
        )?;
        ensure!(
            caps["idempotentSend"] == true && caps["recipientPrecondition"] == true,
            "mailer protocol 2 required"
        );
        let prefix = format!(
            "{} control --state {} worker --credential {} --attempt {} --epoch {}",
            quote(config.supervisor_binary.to_str().unwrap()),
            quote(store.dir.to_str().unwrap()),
            quote(
                dir.join("worker.token")
                    .to_str()
                    .context("credential UTF8")?
            ),
            quote(attempt),
            epoch
        );
        let assignment = if let Some(review) = &order.review {
            format!(
                "You are the independent reviewer, not the author of this task. Inspect the exact candidate {} from base {} and assess every criterion in the original contract. Do not edit or integrate it. Submit method submit_review with attempt, epoch, integration {}, candidate_oid, contract_hash, approve (boolean), criteria (one object per zero-based criterion index, with passed and evidence), and actionable findings. The check receipt digest for this assignment is {}.",
                review.candidate_oid, review.base_oid, review.integration, review.checks_digest
            )
        } else {
            "Execute only the assigned contract. Submit a result with its exact contract_hash and commit/artifact hashes; do not integrate or decide other tasks. You may propose memory with evidence; approval belongs to the supervisor.".into()
        };
        let body = format!(
            "Read your immutable assignment and selected memory with: {prefix} context\nReport readiness using worker apply with method worker_ready, attempt {attempt}, epoch {epoch}, session {} and a stable operation ID. {assignment} Use {prefix} receipt EVENT to distinguish queued from controller-applied. Worker apply accepts --operation ID --request JSON_FILE.",
            plan.expected_session.as_ref().unwrap()
        );
        plan.bootstrap = Some(super::api::call_json(
            &config.mailer_state.join("daemon.sock"),
            &json!({"op":"send","token":token,"recipient":receiver["agent"],"body":body,"operationId":format!("bootstrap-{attempt}-{epoch}"),"recipientSession":receiver["id"],"recipientEpoch":receiver["epoch"]}),
        )?);
        save(store, attempt, &plan)?;
    }
    store.connection.execute(
        "UPDATE jobs SET state='bootstrapped' WHERE id=?1",
        [attempt],
    )?;
    Ok(
        json!({"state":"bootstrapped","native_session":plan.expected_session,"message":plan.bootstrap,"model_ready":false}),
    )
}

/// Executed only by the owned pane wrapper, never through a worker tool. The
/// one-shot claim survives a spawn failure or lost Herdr response.
pub fn run_owned(state: &Path, attempt: &str) -> Result<()> {
    let _lease = super::api::lock_wait(state, "execution.lock")?;
    let connection = rusqlite::Connection::open(state.join("node.sqlite"))?;
    connection.busy_timeout(Duration::from_secs(10))?;
    connection.execute_batch("PRAGMA synchronous=FULL;")?;
    let (epoch, job_state, doc, checkpoint): (u64, String, String, String) = connection.query_row(
        "SELECT epoch,state,doc,checkpoint FROM jobs WHERE id=?1",
        [attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let order: ExecuteOrder = serde_json::from_str(&doc)?;
    let mut plan: LaunchPlan = serde_json::from_str(&checkpoint)?;
    ensure!(
        job_state == "launching"
            && epoch == order.attempt.epoch
            && plan.launch_started
            && !plan.wrapper_started,
        "launcher fenced or already claimed"
    );
    // Build invocation without possessing an owner key or opening controller state.
    let grant_raw: String = connection.query_row(
        "SELECT record FROM commands WHERE id=(SELECT command FROM jobs WHERE id=?1)",
        [attempt],
        |r| r.get(0),
    )?;
    let signed: super::identity::SignedRecord = serde_json::from_str(&grant_raw)?;
    signed.verify(jiff::Timestamp::now().as_second())?;
    let invocation = invocation(state, &order, &plan)?;
    plan.wrapper_started = true;
    plan.wrapper_pid = Some(std::process::id());
    connection.execute(
        "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
        params![attempt, serde_json::to_string(&plan)?],
    )?;
    let cmd = command(invocation);
    super::process::run(
        connection,
        _lease,
        attempt,
        epoch,
        signed.record.expires_at,
        cmd,
    )
}

pub fn read_config(path: &Path) -> Result<RuntimeConfig> {
    ensure!(
        std::fs::metadata(path)?.len() <= 262144,
        "runtime config too large"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn command(invocation: Invocation) -> std::process::Command {
    let mut cmd = std::process::Command::new(invocation.program);
    cmd.args(invocation.argv).current_dir(invocation.cwd);
    for key in [
        "HERDR_MAILER_CONFIG",
        "HERDR_MAILER_CREDENTIAL",
        "HERDR_MAILER_STATE",
        "HERDR_SUPERVISOR_CREDENTIAL",
        "HERDR_SUPERVISOR_STATE",
        "HERDR_SUPERVISOR_ATTEMPT",
        "HERDR_SUPERVISOR_EPOCH",
        "HERDR_SUPERVISOR_RUN",
        "HERDR_SUPERVISOR_GENERATION",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(invocation.environment);
    cmd
}
