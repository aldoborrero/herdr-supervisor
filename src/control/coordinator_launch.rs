//! Controller-host coordinator launcher. Intent precedes Herdr/mailer effects;
//! an uncertain create, registration or spawn never becomes a second launch.
use super::{
    api, coordinator,
    model::*,
    runtime,
    store::{self, Store, now, own, private_dir},
};
use crate::{herdr::Herdr, remote::quote, runner::Runner};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub herdr_binary: PathBuf,
    pub herdr_socket: PathBuf,
    pub supervisor_binary: PathBuf,
    pub mailer_state: PathBuf,
    pub mailer_admin_credential: PathBuf,
    pub codex_binary: PathBuf,
    pub mailer_runtime_binary: PathBuf,
    pub claude_development_channel: bool,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        for path in [
            &self.herdr_binary,
            &self.herdr_socket,
            &self.supervisor_binary,
            &self.mailer_state,
            &self.mailer_admin_credential,
            &self.codex_binary,
            &self.mailer_runtime_binary,
        ] {
            ensure!(
                path.is_absolute(),
                "absolute coordinator launch paths required"
            );
        }
        Ok(())
    }
}
pub fn read_config(path: &Path) -> Result<Config> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "coordinator launch config too large");
    let config: Config = serde_json::from_slice(&bytes)?;
    config.validate()?;
    Ok(config)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub run: String,
    pub generation: u64,
    pub profile_name: String,
    pub profile: Profile,
    pub config: Config,
    pub credential: String,
    pub deadline: i64,
    pub workspace_started: bool,
    #[serde(default)]
    pub workspace_result: Option<WorkspaceResult>,
    pub pane: Option<super::executor::PaneBinding>,
    pub launch_started: bool,
    pub wrapper_started: bool,
    pub expected_session: Option<String>,
    pub registration_started: bool,
    pub registration: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WorkspaceResult {
    Created { receipt: crate::herdr::Created },
    NotStarted { error: String },
    Uncertain { error: String },
}

pub fn fence(state: &Path, command: &Mutation) -> Result<Option<api::Lease>> {
    if matches!(
        command,
        Mutation::StartCoordinator { .. }
            | Mutation::BindSupervisor { .. }
            | Mutation::BindCoordinator { .. }
            | Mutation::Cancel { .. }
            | Mutation::CompleteRun { .. }
    ) {
        // Owner/API mutations fail retryably before any SQL change while an
        // external launch effect is in flight. Do not block an async API thread.
        Ok(Some(api::lock(state, "coordinator-launch.lock")?))
    } else {
        Ok(None)
    }
}

pub(super) fn reserve(
    tx: &Transaction<'_>,
    principal: &Principal,
    command: &Mutation,
) -> Result<Value> {
    own(principal)?;
    let Mutation::StartCoordinator {
        run,
        expected_revision,
        profile,
        config,
    } = command
    else {
        bail!("not a coordinator launch")
    };
    config.validate()?;
    let r: Run = store::document(tx, "runs", run)?;
    ensure!(
        r.state == "active" && r.deadline > now(),
        "run is not active"
    );
    ensure!(r.revision == *expected_revision, "run revision changed");
    let chosen = r
        .policy
        .profiles
        .get(profile)
        .context("coordinator profile not authorized")?
        .clone();
    runtime::validate_args(&chosen.runtime, &chosen.argv)?;
    ensure!(
        chosen.runtime != "claude" || config.claude_development_channel,
        "Claude development channel requires owner opt-in"
    );
    let occupied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_launches WHERE run=?1 AND state<>'exited') OR EXISTS(SELECT 1 FROM coordinator_routes WHERE run=?1 AND active=1)",[run],|r|r.get(0))?;
    ensure!(
        !occupied,
        "coordinator exists or its exit is unresolved; do not launch a replacement"
    );
    let bound = store::mutate(
        tx,
        principal,
        &Mutation::BindSupervisor {
            run: run.clone(),
            expected_revision: *expected_revision,
        },
    )?;
    let plan = Plan {
        id: id(),
        run: run.clone(),
        generation: bound["generation"].as_u64().context("generation")?,
        profile_name: profile.clone(),
        profile: chosen.clone(),
        config: config.clone(),
        credential: bound["credential"].as_str().context("credential")?.into(),
        deadline: r.deadline,
        workspace_started: false,
        workspace_result: None,
        pane: None,
        launch_started: false,
        wrapper_started: false,
        expected_session: (chosen.runtime != "codex").then(id),
        registration_started: false,
        registration: None,
    };
    tx.execute("INSERT INTO coordinator_launches(id,run,generation,state,doc) VALUES (?1,?2,?3,'reserved',?4)",params![plan.id,run,plan.generation,serde_json::to_string(&plan)?])?;
    Ok(
        json!({"id":plan.id,"run":run,"generation":plan.generation,"revision":bound["revision"],"state":"reserved","model_ready":false}),
    )
}

fn plan(c: &Connection, id: &str) -> Result<(String, Plan)> {
    let (state, doc): (String, String) = c.query_row(
        "SELECT state,doc FROM coordinator_launches WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok((state, serde_json::from_str(&doc)?))
}
fn save(c: &Connection, p: &Plan, state: &str) -> Result<()> {
    c.execute(
        "UPDATE coordinator_launches SET state=?2,doc=?3,last_error='' WHERE id=?1",
        params![p.id, state, serde_json::to_string(p)?],
    )?;
    Ok(())
}
pub fn files(state: &Path, p: &Plan) -> PathBuf {
    state
        .join("coordinators")
        .join(&p.run)
        .join(p.generation.to_string())
}
pub(super) fn current(c: &Connection, p: &Plan) -> Result<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM principals p JOIN runs r ON r.id=p.run WHERE p.run=?1 AND p.role='supervisor' AND p.generation=?2 AND p.active=1 AND r.state IN ('active','paused') AND json_extract(r.doc,'$.deadline')>?3)",params![p.run,p.generation,now()],|r|r.get(0)).map_err(Into::into)
}
pub fn summaries(c: &Connection, run: &str) -> Result<Value> {
    let mut q=c.prepare("SELECT l.id,l.generation,l.state,l.last_error,l.doc,p.doc FROM coordinator_launches l LEFT JOIN coordinator_processes p ON p.launch=l.id WHERE l.run=?1 ORDER BY l.generation")?;
    let rows = q.query_map([run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (id, generation, state, error, doc, receipt) = row?;
        let p: Plan = serde_json::from_str(&doc)?;
        let retirement: Option<String> = c
            .query_row(
                "SELECT state FROM coordinator_retirements WHERE launch=?1",
                [&id],
                |row| row.get(0),
            )
            .optional()?;
        let recoveries: u64 = c.query_row(
            "SELECT count(*) FROM registration_recoveries WHERE target=?1",
            [&id],
            |r| r.get(0),
        )?;
        result.push(json!({"id":id,"generation":generation,"state":state,"workspace_retirement":retirement,"workspace_started":p.workspace_started,"workspace_result":p.workspace_result,"profile":p.profile_name,"runtime":p.profile.runtime,"native_session":p.expected_session,"pane":p.pane,"last_error":error,"process_receipt":receipt.map(|s|serde_json::from_str::<Value>(&s)).transpose()?,"registration_recovery_count":recoveries,"registration_state":if p.registration.is_some(){"registered"}else if p.registration_started{"uncertain"}else if recoveries > 0{"retry_authorized"}else{"not_started"}}));
    }
    Ok(json!(result))
}

fn ensure_files(store: &Store, p: &Plan) -> Result<()> {
    let base = store.dir.join("coordinators");
    private_dir(&base)?;
    private_dir(&base.join(&p.run))?;
    let dir = files(&store.dir, p);
    private_dir(&dir)?;
    private_dir(&dir.join("home"))?;
    let credential = coordinator::credential_path(&store.dir, &p.run, p.generation);
    runtime::ensure_file(&credential, p.credential.as_bytes())?;
    let command = format!(
        "{} control --state {} context --credential {} {}",
        quote(p.config.supervisor_binary.to_str().context("binary UTF8")?),
        quote(store.dir.to_str().context("state UTF8")?),
        quote(credential.to_str().context("credential UTF8")?),
        quote(&p.run)
    );
    runtime::ensure_file(&dir.join("home/AGENTS.md"),format!("# Scoped coordinator\n\nRead your current run with `{command}`. Use this generation's capability for all decisions. Await the mailer bootstrap if no objective has arrived. Never read or use the operator's admin credential.\n\n{}",include_str!("../../skill/MANAGED_COORDINATOR.md")).as_bytes())?;
    runtime::ensure_file(
        &dir.join("home/CLAUDE.md"),
        b"Read AGENTS.md in this directory.\n",
    )?;
    Ok(())
}

fn registration(store: &Store, p: &mut Plan) -> Result<()> {
    let session = p
        .expected_session
        .clone()
        .context("native UUID not observed")?;
    let dir = files(&store.dir, p);
    let receipt = dir.join("registration.json");
    if p.registration.is_none() {
        let value = if receipt.try_exists()? {
            serde_json::from_str::<Value>(&runtime::credential(&receipt)?)?
        } else {
            ensure!(
                !p.registration_started,
                "mailer registration outcome is uncertain; no automatic re-registration"
            );
            p.registration_started = true;
            save(&store.connection, p, "launching")?;
            let value = api::call_json(
                &p.config.mailer_state.join("daemon.sock"),
                &json!({"op":"register","token":runtime::credential(&p.config.mailer_admin_credential)?,"agent":format!("hs-coord-{}",p.id),"runtime":p.profile.runtime,"location":session}),
            )?;
            runtime::ensure_file(&receipt, &serde_json::to_vec(&value)?)?;
            value
        };
        let s = &value["session"];
        ensure!(
            s["agent"] == format!("hs-coord-{}", p.id)
                && s["runtime"] == p.profile.runtime
                && s["location"] == session,
            "mailer returned a different coordinator"
        );
        let endpoint = coordinator::Endpoint {
            agent: s["agent"].as_str().context("agent")?.into(),
            mailer_session: s["id"].as_str().context("session")?.into(),
            mailer_epoch: s["epoch"].as_u64().context("epoch")?,
            runtime: p.profile.runtime.clone(),
            native_session: session.clone(),
        };
        endpoint.validate()?;
        let token = value["token"].as_str().context("mailer token missing")?;
        ensure!((32..=256).contains(&token.len()), "invalid mailer token");
        runtime::ensure_file(&dir.join("mailer.token"), token.as_bytes())?;
        p.registration = Some(s.clone());
        save(&store.connection, p, "launching")?;
    }
    if p.profile.runtime != "codex" {
        runtime::ensure_file(
            &dir.join("mailer.json"),
            &serde_json::to_vec(
                &json!({"runtime":p.profile.runtime,"location":session,"state":p.config.mailer_state,"credential":dir.join("mailer.token"),"receipts":dir.join("native-receipts")}),
            )?,
        )?;
    }
    Ok(())
}

fn observe_shell(herdr: &Herdr, p: &Plan, home: &Path) -> Result<super::executor::PaneBinding> {
    let panes = herdr.pane_list()?;
    let matches = panes
        .iter()
        .filter(|pane| Path::new(&pane.cwd) == home)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "coordinator workspace creation unresolved or ambiguous"
    );
    let pane = matches[0];
    if let Some(WorkspaceResult::Created { receipt }) = &p.workspace_result {
        ensure!(
            pane.workspace_id == receipt.workspace_id
                && pane.tab_id == receipt.tab_id
                && pane.pane_id == receipt.pane_id,
            "coordinator workspace differs from creation receipt"
        );
    }
    ensure!(
        !pane.workspace_id.is_empty() && !pane.pane_id.is_empty() && !pane.terminal_id.is_empty(),
        "Herdr omitted coordinator identity"
    );
    ensure!(
        panes
            .iter()
            .filter(|x| x.workspace_id == pane.workspace_id)
            .count()
            == 1,
        "coordinator workspace has additional panes"
    );
    ensure!(
        herdr
            .agent_list()?
            .iter()
            .all(|a| a.pane_id != pane.pane_id)
            && herdr.pane_idle_shell(&pane.pane_id),
        "coordinator pane is occupied"
    );
    let binding = super::executor::PaneBinding {
        workspace: pane.workspace_id.clone(),
        pane: pane.pane_id.clone(),
        terminal: pane.terminal_id.clone(),
    };
    if let Some(old) = &p.pane {
        ensure!(old == &binding, "coordinator pane identity changed");
    }
    Ok(binding)
}

pub fn advance(store: &mut Store, id: &str, runner: &dyn Runner) -> Result<Value> {
    let _lease = api::lock(&store.dir, "coordinator-launch.lock")?;
    let (state, mut p) = plan(&store.connection, id)?;
    if state == "exited" {
        return Ok(json!({"id":id,"state":state}));
    }
    super::process_recovery::recover(&store.connection, &store.dir, id, true)?;
    let exited: bool = store.connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM coordinator_processes WHERE launch=?1 AND state='exited')",
        [id],
        |r| r.get(0),
    )?;
    let active = current(&store.connection, &p)?;
    if exited || (!active && !p.wrapper_started) {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        save(&tx, &p, "exited")?;
        tx.execute(
            "UPDATE principals SET active=0 WHERE role='supervisor' AND run=?1 AND generation=?2",
            params![p.run, p.generation],
        )?;
        tx.execute(
            "UPDATE coordinator_routes SET active=0 WHERE run=?1 AND generation=?2",
            params![p.run, p.generation],
        )?;
        tx.execute("UPDATE coordinator_notices SET state='superseded' WHERE run=?1 AND generation=?2 AND state IN ('pending','retry')",params![p.run,p.generation])?;
        tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('controller-launcher',?1,?2,'coordinator_exited',?3,?4)",params![format!("exit-{}",p.id),p.run,serde_json::to_string(&json!({"launch":p.id,"generation":p.generation,"process_exit_receipt":exited,"fenced_before_spawn":!p.wrapper_started}))?,now()])?;
        tx.commit()?;
        return Ok(json!({"id":id,"state":"exited"}));
    }
    ensure!(
        active,
        "coordinator generation fenced; waiting for owned process exit"
    );
    if state == "bound" {
        return Ok(json!({"id":id,"state":state,"model_ready":false}));
    }
    ensure_files(store, &p)?;
    let home = files(&store.dir, &p).join("home");
    let herdr = Herdr::new(
        p.config.herdr_binary.to_string_lossy(),
        &p.config.herdr_socket,
        runner,
    );
    if !p.workspace_started {
        ensure!(
            herdr
                .pane_list()?
                .iter()
                .all(|pane| Path::new(&pane.cwd) != home),
            "unexpected pane already uses the reserved coordinator directory"
        );
        p.workspace_started = true;
        // Clear an earlier proven pre-spawn failure before a new attempt. A
        // crash after this save must be treated as an unknown outcome.
        p.workspace_result = None;
        save(&store.connection, &p, "preparing")?;
        // A lost response remains an intent to inspect the unique cwd. Never
        // issue workspace.create a second time merely because no pane was seen.
        match herdr.workspace_create(&home, &format!("supervisor {}", p.run), false) {
            Ok(receipt) => p.workspace_result = Some(WorkspaceResult::Created { receipt }),
            Err(error) => {
                let not_started = error.not_started();
                let error = error.to_string().replace(&p.credential, "[redacted]");
                // Bound public diagnostics without cutting a UTF-8 codepoint.
                let error: String = error.chars().take(2048).collect();
                p.workspace_result = Some(if not_started {
                    p.workspace_started = false;
                    WorkspaceResult::NotStarted { error }
                } else {
                    WorkspaceResult::Uncertain { error }
                });
            }
        }
        save(&store.connection, &p, "preparing")?;
    }
    if let Some(WorkspaceResult::NotStarted { error }) = &p.workspace_result {
        bail!("coordinator workspace command did not start; retry is safe: {error}");
    }
    if !p.launch_started {
        p.pane = Some(observe_shell(&herdr, &p, &home).map_err(|error| {
            let cause = match &p.workspace_result {
                Some(WorkspaceResult::Uncertain { error }) => error.as_str(),
                Some(WorkspaceResult::Created { .. }) => "creation receipt saved",
                _ => "no durable creation result (interrupted or legacy launch)",
            };
            anyhow::anyhow!(
                "{error}; workspace create: {cause}; creation will not be repeated. To abandon this launch, the owner can bind_supervisor at the current run revision, wait for exited, then start a new coordinator"
            )
        })?);
        save(&store.connection, &p, "preparing")?;
        if p.profile.runtime != "codex" {
            registration(store, &mut p)?;
        }
        let pane = p.pane.as_ref().context("pane binding")?.pane.clone();
        let command = format!(
            "{} control --state {} run-coordinator-runtime {}",
            quote(p.config.supervisor_binary.to_str().context("binary UTF8")?),
            quote(store.dir.to_str().context("state UTF8")?),
            quote(&p.id)
        );
        p.launch_started = true;
        save(&store.connection, &p, "launching")?;
        let _outcome = herdr.call(&["pane", "run", &pane, &command], Duration::from_secs(10));
        return Ok(json!({"id":id,"state":"launching","model_ready":false}));
    }
    let binding = p.pane.as_ref().context("pane binding")?;
    let agents = herdr.agent_list()?;
    let matches = agents
        .iter()
        .filter(|a| a.pane_id == binding.pane)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "coordinator launch unresolved; no second launch"
    );
    let agent = matches[0];
    ensure!(
        p.wrapper_started
            && agent.terminal_id == binding.terminal
            && agent.workspace_id == binding.workspace
            && agent.agent == p.profile.runtime
            && agent.works_in(home.to_str().context("home UTF8")?),
        "coordinator native identity differs"
    );
    ensure!(
        herdr
            .pane_list()?
            .iter()
            .any(|pane| pane.pane_id == binding.pane
                && pane.workspace_id == binding.workspace
                && pane.terminal_id == binding.terminal
                && (Path::new(&pane.cwd) == home || Path::new(&pane.foreground_cwd) == home)),
        "coordinator terminal identity changed"
    );
    let session = agent.native_session_id()?;
    if let Some(expected) = &p.expected_session {
        ensure!(expected == &session, "coordinator native UUID changed");
    }
    p.expected_session = Some(session.clone());
    save(&store.connection, &p, "launching")?;
    registration(store, &mut p)?;
    let s = p
        .registration
        .as_ref()
        .context("mailer registration missing")?;
    let endpoint = coordinator::Endpoint {
        agent: s["agent"].as_str().context("agent")?.into(),
        mailer_session: s["id"].as_str().context("session")?.into(),
        mailer_epoch: s["epoch"].as_u64().context("epoch")?,
        runtime: p.profile.runtime.clone(),
        native_session: session,
    };
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure!(current(&tx, &p)?, "coordinator retired before binding");
    coordinator::activate(&tx, &p.run, p.generation, &p.credential, &endpoint)?;
    save(&tx, &p, "bound")?;
    tx.commit()?;
    Ok(json!({"id":id,"state":"bound","model_ready":false}))
}

pub fn advance_all(
    store: &mut Store,
    notification: &coordinator::Config,
    runner: &dyn Runner,
) -> Result<()> {
    // Bound or unresolved coordinators cannot starve later launch intents.
    let cursor: i64 = store
        .connection
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='coordinator_launch_cursor'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let ids = {
        let mut q=store.connection.prepare("SELECT rowid,id FROM coordinator_launches WHERE state<>'exited' ORDER BY (rowid<=?1),rowid LIMIT 128")?;
        q.query_map([cursor], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rowid, id) in ids {
        store.connection.execute("INSERT INTO meta(key,value) VALUES ('coordinator_launch_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[rowid.to_string()])?;
        let (_, p) = plan(&store.connection, &id)?;
        let result = if p.config.mailer_state == notification.mailer_state {
            advance(store, &id, runner)
        } else {
            Err(anyhow::anyhow!(
                "coordinator launch and notifier must use the same mailer state"
            ))
        };
        if let Err(error) = result {
            // External diagnostics may contain paths; never expose capabilities.
            let message = error.to_string().replace(&p.credential, "[redacted]");
            store.connection.execute(
                "UPDATE coordinator_launches SET last_error=?2 WHERE id=?1",
                params![id, message],
            )?;
        }
    }
    Ok(())
}

pub fn invocation(state: &Path, p: &Plan) -> Result<runtime::Invocation> {
    let dir = files(state, p);
    let environment = BTreeMap::from([
        (
            "HERDR_SUPERVISOR_STATE".into(),
            state.to_string_lossy().into(),
        ),
        ("HERDR_SUPERVISOR_RUN".into(), p.run.clone()),
        (
            "HERDR_SUPERVISOR_GENERATION".into(),
            p.generation.to_string(),
        ),
        (
            "HERDR_SUPERVISOR_CREDENTIAL".into(),
            coordinator::credential_path(state, &p.run, p.generation)
                .to_string_lossy()
                .into(),
        ),
        (
            "HERDR_MAILER_STATE".into(),
            p.config.mailer_state.to_string_lossy().into(),
        ),
        (
            "HERDR_MAILER_CREDENTIAL".into(),
            dir.join("mailer.token").to_string_lossy().into(),
        ),
    ]);
    runtime::native_invocation(
        &p.profile,
        &runtime::Programs {
            codex_binary: p.config.codex_binary.clone(),
            mailer_runtime_binary: p.config.mailer_runtime_binary.clone(),
            claude_development_channel: p.config.claude_development_channel,
        },
        &dir,
        &dir.join("home"),
        environment,
    )
}

pub fn run_owned(state: &Path, id: &str) -> Result<()> {
    let lease = api::lock_wait(state, "coordinator-launch.lock")?;
    let store = Store::open(state)?;
    let (status, mut p) = plan(&store.connection, id)?;
    ensure!(
        status == "launching"
            && p.launch_started
            && !p.wrapper_started
            && current(&store.connection, &p)?,
        "coordinator wrapper fenced or already claimed"
    );
    let invocation = invocation(state, &p)?;
    p.wrapper_started = true;
    save(&store.connection, &p, "launching")?;
    super::process::run_coordinator(
        store.connection,
        lease,
        id,
        p.generation,
        p.deadline,
        runtime::command(invocation),
    )
}
