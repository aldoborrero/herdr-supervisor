//! Retire only an owned coordinator workspace. Files and history remain intact;
//! process quiescence is an explicit local owner attestation, not inferred idle.
use std::{path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    coordinator_launch::{self, Plan},
    executor::PaneBinding,
    model::digest,
    store::{self, Store, now},
};
use crate::{
    herdr::Herdr,
    runner::{RealRunner, Runner},
};

pub const SCHEMA: &str = "
CREATE TABLE coordinator_retirements (
 launch TEXT PRIMARY KEY REFERENCES coordinator_launches(id), operation TEXT NOT NULL UNIQUE,
 request TEXT NOT NULL, scope_hash TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','closing','closed')),
 receipt TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT;
PRAGMA user_version=13;";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub launch: String,
    pub expected_generation: u64,
    pub expected_binding: PaneBinding,
    pub processes_confirmed_stopped: bool,
    pub retain_files_close_workspace: bool,
    pub reason: String,
}

#[derive(Serialize)]
struct Scope {
    plan: Plan,
    process: Option<(u64, String, String)>,
}

fn authority(c: &Connection, token: &str) -> Result<String> {
    let principal = store::authenticate(c, token)?;
    store::own(&principal)?;
    Ok(principal.id)
}

fn scope(c: &Connection, r: &Request) -> Result<Scope> {
    let (run, generation, state, doc): (String, u64, String, String) = c.query_row(
        "SELECT run,generation,state,doc FROM coordinator_launches WHERE id=?1",
        [&r.launch],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let plan: Plan = serde_json::from_str(&doc)?;
    uuid::Uuid::parse_str(&run)?;
    ensure!(
        state == "exited"
            && plan.id == r.launch
            && plan.run == run
            && generation == r.expected_generation
            && plan.generation == generation
            && plan.workspace_started
            && plan.pane.as_ref() == Some(&r.expected_binding),
        "coordinator must be exited with the exact owned generation and resource binding"
    );
    plan.config.validate()?;
    let active: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM principals WHERE run=?1 AND generation=?2 AND role='supervisor' AND active=1) OR EXISTS(SELECT 1 FROM coordinator_routes WHERE run=?1 AND generation=?2 AND active=1)",
        params![run,generation], |row| row.get(0))?;
    ensure!(!active, "coordinator authority has not been retired");
    let process: Option<(u64, String, String)> = c
        .query_row(
            "SELECT generation,state,doc FROM coordinator_processes WHERE launch=?1",
            [&r.launch],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((epoch, state, doc)) = &process {
        let receipt: Value = serde_json::from_str(doc)?;
        ensure!(
            *epoch == generation
                && state == "exited"
                && plan.wrapper_started
                && receipt["launch"] == r.launch
                && receipt["generation"] == generation
                && receipt["scope"] == "native_process",
            "exact native process exit receipt required"
        );
    } else {
        ensure!(
            !plan.wrapper_started,
            "coordinator wrapper exit remains unresolved"
        );
    }
    Ok(Scope { plan, process })
}

fn hash(s: &Scope) -> Result<String> {
    Ok(digest(&serde_json::to_vec(s)?))
}

fn retained_home(root: &Path, s: &Scope) -> Result<std::path::PathBuf> {
    let path = coordinator_launch::files(root, &s.plan).join("home");
    let expected = std::fs::canonicalize(root)?
        .join("coordinators")
        .join(&s.plan.run)
        .join(s.plan.generation.to_string())
        .join("home");
    ensure!(
        std::fs::canonicalize(&path)? == expected && std::fs::symlink_metadata(&path)?.is_dir(),
        "coordinator home identity changed"
    );
    Ok(path)
}

fn inspect(herdr: &Herdr, root: &Path, s: &Scope, r: &Request) -> Result<()> {
    let home = retained_home(root, s)?;
    let b = &r.expected_binding;
    let workspaces = herdr.workspace_list()?;
    let matching: Vec<_> = workspaces
        .iter()
        .filter(|w| w.workspace_id == b.workspace)
        .collect();
    ensure!(
        matching.len() == 1 && matching[0].pane_count == 1 && matching[0].worktree.is_none(),
        "coordinator workspace is missing, changed or contains other resources"
    );
    let panes = herdr.pane_list()?;
    let matching: Vec<_> = panes
        .iter()
        .filter(|p| {
            p.workspace_id == b.workspace || p.pane_id == b.pane || p.terminal_id == b.terminal
        })
        .collect();
    ensure!(
        matching.len() == 1
            && matching[0].workspace_id == b.workspace
            && matching[0].pane_id == b.pane
            && matching[0].terminal_id == b.terminal
            && Path::new(&matching[0].cwd) == home
            && (matching[0].foreground_cwd.is_empty()
                || Path::new(&matching[0].foreground_cwd) == home),
        "coordinator pane identity or working directory changed"
    );
    ensure!(
        herdr
            .agent_list()?
            .iter()
            .all(|a| a.workspace_id != b.workspace
                && a.pane_id != b.pane
                && a.terminal_id != b.terminal)
            && herdr.pane_idle_shell(&b.pane),
        "coordinator native resources are occupied or unknown"
    );
    Ok(())
}

fn absent(herdr: &Herdr, r: &Request) -> Result<bool> {
    let b = &r.expected_binding;
    Ok(herdr
        .workspace_list()?
        .iter()
        .all(|w| w.workspace_id != b.workspace)
        && herdr.pane_list()?.iter().all(|p| {
            p.workspace_id != b.workspace && p.pane_id != b.pane && p.terminal_id != b.terminal
        })
        && herdr.agent_list()?.iter().all(|a| {
            a.workspace_id != b.workspace && a.pane_id != b.pane && a.terminal_id != b.terminal
        }))
}

fn journal(c: &Connection, principal: &str, r: &Request, kind: &str, doc: &Value) -> Result<()> {
    c.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES (?1,?2,?3,?4,?5,?6)",
        params![principal,r.operation,r.launch,kind,doc.to_string(),now()])?;
    Ok(())
}

pub fn retire(store: &mut Store, token: &str, r: &Request) -> Result<Value> {
    retire_with(store, token, r, &RealRunner)
}

pub(super) fn retire_with(
    store: &mut Store,
    token: &str,
    r: &Request,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(&r.operation)?;
    uuid::Uuid::parse_str(&r.launch)?;
    ensure!(
        r.expected_generation > 0
            && r.processes_confirmed_stopped
            && r.retain_files_close_workspace,
        "explicit process/retention confirmations and generation required"
    );
    for id in [
        &r.expected_binding.workspace,
        &r.expected_binding.pane,
        &r.expected_binding.terminal,
    ] {
        ensure!(
            !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
            "invalid native resource identity"
        );
    }
    ensure!(
        !r.reason.trim().is_empty()
            && r.reason.len() <= 4096
            && !r.reason.chars().any(char::is_control),
        "a bounded single-line operator reason is required"
    );
    authority(&store.connection, token)?;
    let _lease = super::api::lock(&store.dir, "coordinator-launch.lock")?;
    let frozen = serde_json::to_string(r)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    authority(&tx, token)?;
    let prior: Option<(String,String,String,Option<String>)> = tx.query_row(
        "SELECT request,scope_hash,state,receipt FROM coordinator_retirements WHERE launch=?1 OR operation=?2",
        params![r.launch,r.operation], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    if let Some((saved, _, state, receipt)) = &prior {
        ensure!(saved == &frozen, "coordinator retirement decision changed");
        if state == "closed" {
            return Ok(serde_json::from_str(
                receipt.as_deref().context("missing retirement receipt")?,
            )?);
        }
    }
    let s = scope(&tx, r)?;
    let scope_hash = hash(&s)?;
    if let Some((_, saved, _, _)) = &prior {
        ensure!(
            saved == &scope_hash,
            "coordinator retirement authority changed"
        );
    }
    tx.commit()?;
    let home = retained_home(&store.dir, &s)?;
    let herdr = Herdr::new(
        s.plan.config.herdr_binary.to_string_lossy(),
        &s.plan.config.herdr_socket,
        runner,
    );
    if !prior.as_ref().is_some_and(|p| p.2 == "closing") {
        inspect(&herdr, &store.dir, &s, r)?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, r)?)? == scope_hash,
            "coordinator authority changed during inspection"
        );
        if prior.is_none() {
            tx.execute(
                "INSERT INTO coordinator_retirements VALUES (?1,?2,?3,?4,'prepared',NULL,?5,?5)",
                params![r.launch, r.operation, frozen, scope_hash, now()],
            )?;
            journal(
                &tx,
                &principal,
                r,
                "coordinator_retirement_prepared",
                &serde_json::to_value(r)?,
            )?;
        }
        tx.commit()?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, r)?)? == scope_hash,
            "coordinator authority changed before close"
        );
        ensure!(tx.execute("UPDATE coordinator_retirements SET state='closing',updated=?2 WHERE launch=?1 AND state='prepared'",
            params![r.launch,now()])? == 1, "retirement phase changed");
        journal(
            &tx,
            &principal,
            r,
            "coordinator_retirement_issued",
            &json!({"workspace":r.expected_binding.workspace}),
        )?;
        tx.commit()?;
        let _outcome = herdr.call(
            &["workspace", "close", &r.expected_binding.workspace],
            Duration::from_secs(30),
        );
    }
    retained_home(&store.dir, &s)?;
    if absent(&herdr, r).unwrap_or(false) {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, r)?)? == scope_hash,
            "coordinator authority changed after close"
        );
        let receipt = json!({"launch":r.launch,"generation":r.expected_generation,"operation":r.operation,"state":"closed",
            "workspace_closed":true,"files_deleted":false,"retained_home":home,"binding":r.expected_binding,"observed_at":now(),
            "process_evidence":"local_operator_attestation_and_native_exit_or_fenced_before_spawn"});
        ensure!(tx.execute("UPDATE coordinator_retirements SET state='closed',receipt=?2,updated=?3 WHERE launch=?1 AND state='closing'",
            params![r.launch,receipt.to_string(),now()])? == 1, "retirement phase changed");
        journal(
            &tx,
            &principal,
            r,
            "coordinator_retirement_completed",
            &receipt,
        )?;
        tx.commit()?;
        return Ok(receipt);
    }
    Ok(
        json!({"launch":r.launch,"operation":r.operation,"state":"closing","workspace_closed":null,"files_deleted":false,
        "outcome":"unresolved","automatic_retry":false}),
    )
}
