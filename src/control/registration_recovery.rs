//! Explicit local maintenance for a lost registration before receiver binding.
//! Reauthorization is transactional and replayable; it performs no remote effect.
use std::{path::Path, time::Duration};

use crate::{
    herdr::Herdr,
    runner::{RealRunner, Runner},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

use super::{coordinator_launch, model::identifier, runtime::LaunchPlan, store::now};

pub const SCHEMA: &str = "CREATE TABLE registration_recoveries (target TEXT NOT NULL, operation TEXT NOT NULL, epoch INTEGER NOT NULL, doc TEXT NOT NULL, PRIMARY KEY(target,operation)) STRICT;";

fn recorded(c: &Connection, id: &str, operation: &str, epoch: u64) -> Result<Option<Value>> {
    let previous: Option<(u64, String)> = c
        .query_row(
            "SELECT epoch,doc FROM registration_recoveries WHERE target=?1 AND operation=?2",
            params![id, operation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((previous_epoch, receipt)) = previous {
        ensure!(
            previous_epoch == epoch,
            "recovery operation content changed"
        );
        return Ok(Some(serde_json::from_str(&receipt)?));
    }
    let count: u64 = c.query_row(
        "SELECT count(*) FROM registration_recoveries WHERE target=?1",
        [id],
        |r| r.get(0),
    )?;
    ensure!(count < 32, "registration recovery history limit reached");
    Ok(None)
}

fn no_receiver_files(dir: &Path) -> Result<()> {
    // Even an invalid/partial receipt must be inspected, never overwritten by
    // changing the registry epoch. Normal advance can consume a valid receipt.
    for name in [
        "registration.json",
        "mailer.token",
        "mailer.json",
        "native-receipts",
    ] {
        match std::fs::symlink_metadata(dir.join(name)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => {
                anyhow::bail!("existing receiver evidence; reconcile it without re-registration")
            }
        }
    }
    Ok(())
}

pub fn recover(state: &Path, node: bool, id: &str, epoch: u64, operation: &str) -> Result<Value> {
    recover_observed(state, node, id, epoch, operation, &RealRunner)
}

pub(super) fn recover_observed(
    state: &Path,
    node: bool,
    id: &str,
    epoch: u64,
    operation: &str,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(id)?;
    identifier(operation)?;
    ensure!(epoch > 0, "recovery epoch must be positive");
    // Opening verifies existing private state and never creates a database.
    let mut c = super::reception::local_db(state, node, true)?;
    let _lease = super::api::lock(
        state,
        if node {
            "execution.lock"
        } else {
            "coordinator-launch.lock"
        },
    )?;
    // Observe without blocking process exit receipts. Before authorization,
    // recheck the exact target under a write transaction. Unrelated writes need
    // not prevent recovery, but changed process/authority evidence does.
    let tx = c.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let schema: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        schema == if node { 10 } else { 11 },
        "unsupported recovery schema; run the current service migration first"
    );
    if let Some(receipt) = recorded(&tx, id, operation, epoch)? {
        return Ok(receipt);
    }
    let original_scope = recovery_scope(&tx, node, id)?;
    let receiver_dir;
    let mut expires_at;
    let mut live_process = None;
    let mut response = json!({"id":id,"node":node,"epoch":epoch,"operation":operation,"recorded_state":"registration_retry_authorized","recorded_at":now(),"registration_attempted":false,"model_launched":false});
    let (new_document, aggregate) = if node {
        let (current_epoch, status, doc, checkpoint, command): (
            u64,
            String,
            String,
            String,
            String,
        ) = tx.query_row(
            "SELECT epoch,state,doc,checkpoint,command FROM jobs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let mut p: LaunchPlan = serde_json::from_str(&checkpoint)?;
        let order: super::node::ExecuteOrder = serde_json::from_str(&doc)?;
        ensure!(
            current_epoch == epoch
                && order.attempt.epoch == epoch
                && order.attempt.id == id
                && status == "launching",
            "attempt is retired or not awaiting registration"
        );
        let (wire, retained_hash): (String, String) = tx.query_row(
            "SELECT record,content_hash FROM commands WHERE id=?1",
            [&command],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let signed: super::identity::SignedRecord = serde_json::from_str(&wire)?;
        signed.verify(now())?;
        expires_at = signed.record.expires_at;
        ensure!(
            signed.content_hash == retained_hash,
            "original assignment integrity differs"
        );
        let (owner, key, node_id, node_epoch): (String, String, String, u64) = tx.query_row(
            "SELECT owner,key,node,epoch FROM authority WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        ensure!(
            signed.record.record_type == "local.herdr.supervisor.command"
                && signed.record.id == command
                && signed.record.owner == owner
                && signed.record.author == owner
                && signed.record.recipient == key
                && signed.record.epoch == node_epoch
                && order.attempt.node == node_id,
            "original assignment authority differs"
        );
        let original: super::node::ExecuteOrder =
            serde_json::from_value(signed.record.payload["body"].clone())?;
        ensure!(
            signed.record.payload["kind"] == "execute"
                && serde_json::to_value(original)? == serde_json::to_value(&order)?,
            "original assignment differs"
        );
        ensure!(
            p.bind_event.is_none()
                && p.bootstrap.is_none()
                && p.registration.is_none()
                && p.registration_started,
            "registration retry requires an unbound receiver with a lost response"
        );
        let session = p
            .expected_session
            .as_deref()
            .context("native UUID missing")?;
        uuid::Uuid::parse_str(session)?;
        receiver_dir = state.join("workers").join(id);
        no_receiver_files(&receiver_dir)?;
        let process: Option<(u64, String, String)> = tx
            .query_row(
                "SELECT epoch,state,doc FROM runtime_processes WHERE attempt=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if order.profile.runtime == "codex" {
            ensure!(
                p.launch_started && p.wrapper_started,
                "Codex recovery requires the original owned launch"
            );
            let receipt = process_receipt(process, id, epoch, false)?;
            ensure!(
                receipt["wrapper_pid"].as_u64() == p.wrapper_pid.map(u64::from),
                "native wrapper identity differs"
            );
            super::process::require_live_owner(state, &receipt)?;
            super::executor::inspect_checkout(runner, &p.checkout)?;
            let herdr = Herdr::new(&p.checkout.herdr_binary, &p.checkout.socket, runner);
            observe_codex(
                &herdr,
                p.checkout
                    .binding
                    .as_ref()
                    .context("native pane binding missing")?,
                &p.checkout.worktree,
                session,
                &receipt,
            )?;
            expires_at = expires_at.min(receipt["deadline"].as_i64().context("process deadline")?);
            live_process = Some(receipt);
            response["stage"] = json!("codex_before_binding");
        } else {
            ensure!(
                ["claude", "pi"].contains(&order.profile.runtime.as_str())
                    && !p.launch_started
                    && !p.wrapper_started
                    && p.wrapper_pid.is_none()
                    && process.is_none(),
                "registration retry requires an unlaunched receiver"
            );
            response["stage"] = json!("before_native_launch");
        }
        p.registration_started = false;
        (serde_json::to_string(&p)?, None)
    } else {
        let (status, doc): (String, String) = tx.query_row(
            "SELECT state,doc FROM coordinator_launches WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut p: coordinator_launch::Plan = serde_json::from_str(&doc)?;
        ensure!(
            p.id == id
                && p.generation == epoch
                && ["preparing", "launching"].contains(&status.as_str())
                && coordinator_launch::current(&tx, &p)?
                && p.deadline > now(),
            "coordinator generation is retired or not awaiting registration"
        );
        ensure!(
            p.registration.is_none() && p.registration_started,
            "registration retry requires an unbound receiver with a lost response"
        );
        let session = p
            .expected_session
            .as_deref()
            .context("native UUID missing")?;
        uuid::Uuid::parse_str(session)?;
        uuid::Uuid::parse_str(&p.run)?;
        expires_at = p.deadline.min(tx.query_row(
            "SELECT json_extract(doc,'$.deadline') FROM runs WHERE id=?1",
            [&p.run],
            |r| r.get::<_, i64>(0),
        )?);
        receiver_dir = coordinator_launch::files(state, &p);
        no_receiver_files(&receiver_dir)?;
        let process: Option<(u64, String, String)> = tx
            .query_row(
                "SELECT generation,state,doc FROM coordinator_processes WHERE launch=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if p.profile.runtime == "codex" {
            ensure!(
                p.launch_started && p.wrapper_started,
                "Codex recovery requires the original owned launch"
            );
            let bound: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM coordinator_routes WHERE run=?1 AND generation=?2)",
                params![p.run, p.generation],
                |r| r.get(0),
            )?;
            ensure!(!bound, "coordinator receiver already bound");
            let receipt = process_receipt(process, id, epoch, true)?;
            super::process::require_live_owner(state, &receipt)?;
            let herdr = Herdr::new(
                p.config.herdr_binary.to_string_lossy(),
                &p.config.herdr_socket,
                runner,
            );
            observe_codex(
                &herdr,
                p.pane.as_ref().context("native pane binding missing")?,
                &receiver_dir.join("home"),
                session,
                &receipt,
            )?;
            expires_at = expires_at.min(receipt["deadline"].as_i64().context("process deadline")?);
            live_process = Some(receipt);
            response["stage"] = json!("codex_before_binding");
        } else {
            ensure!(
                ["claude", "pi"].contains(&p.profile.runtime.as_str())
                    && !p.launch_started
                    && !p.wrapper_started
                    && process.is_none(),
                "registration retry requires an unlaunched receiver"
            );
            response["stage"] = json!("before_native_launch");
        }
        p.registration_started = false;
        (serde_json::to_string(&p)?, Some(p.run))
    };
    tx.commit()?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure!(
        recovery_scope(&tx, node, id)? == original_scope,
        "receiver state changed during inspection; retry against current evidence"
    );
    ensure!(
        expires_at > now(),
        "registration recovery authority expired during inspection"
    );
    no_receiver_files(&receiver_dir)?;
    if let Some(receipt) = &live_process {
        super::process::require_live_owner(state, receipt)?;
    }
    if let Some(run) = aggregate {
        tx.execute(
            "UPDATE coordinator_launches SET doc=?2,last_error='' WHERE id=?1",
            params![id, new_document],
        )?;
        tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('local-maintenance',?1,?2,'registration_recovery_authorized',?3,?4)",params![format!("registration-{id}-{operation}"),run,serde_json::to_string(&response)?,now()])?;
    } else {
        tx.execute(
            "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
            params![id, new_document],
        )?;
    }
    tx.execute(
        "INSERT INTO registration_recoveries VALUES (?1,?2,?3,?4)",
        params![id, operation, epoch, serde_json::to_string(&response)?],
    )?;
    tx.commit()?;
    Ok(response)
}

fn process_receipt(
    process: Option<(u64, String, String)>,
    id: &str,
    epoch: u64,
    coordinator: bool,
) -> Result<Value> {
    let (generation, status, doc) = process.context("owned native process evidence missing")?;
    let receipt: Value = serde_json::from_str(&doc)?;
    ensure!(
        generation == epoch
            && status == "running"
            && receipt[if coordinator { "launch" } else { "attempt" }] == id
            && receipt[if coordinator { "generation" } else { "epoch" }] == epoch
            && receipt["scope"] == "native_process"
            && receipt["deadline"].as_i64().is_some_and(|v| v > now()),
        "native process identity, state or deadline differs"
    );
    ensure!(
        receipt["child_pid"]
            .as_u64()
            .is_some_and(|v| v > 0 && v <= i32::MAX as u64)
            && receipt["wrapper_pid"]
                .as_u64()
                .is_some_and(|v| v > 0 && v <= i32::MAX as u64),
        "native process IDs missing"
    );
    Ok(receipt)
}

fn observe_codex(
    herdr: &Herdr<'_>,
    binding: &super::executor::PaneBinding,
    cwd: &Path,
    session: &str,
    process: &Value,
) -> Result<()> {
    ensure!(
        std::fs::canonicalize(cwd)? == cwd,
        "native directory identity changed"
    );
    let agents = herdr.agent_list()?;
    let matches: Vec<_> = agents
        .iter()
        .filter(|a| a.pane_id == binding.pane)
        .collect();
    ensure!(
        matches.len() == 1,
        "native receiver is missing or ambiguous"
    );
    let agent = matches[0];
    ensure!(
        agent.agent == "codex"
            && agent.workspace_id == binding.workspace
            && agent.terminal_id == binding.terminal
            && agent.session_id() == session
            && agent.works_in(cwd.to_str().context("native directory UTF8")?),
        "native receiver identity differs"
    );
    let panes = herdr.pane_list()?;
    let matches: Vec<_> = panes
        .iter()
        .filter(|p| p.workspace_id == binding.workspace)
        .collect();
    ensure!(
        matches.len() == 1,
        "native workspace is missing or ambiguous"
    );
    let pane = matches[0];
    ensure!(
        pane.pane_id == binding.pane
            && pane.terminal_id == binding.terminal
            && (Path::new(&pane.cwd) == cwd || Path::new(&pane.foreground_cwd) == cwd),
        "native terminal identity differs"
    );
    let result = herdr.call(
        &["pane", "process-info", "--pane", &binding.pane],
        Duration::from_secs(5),
    )?;
    let info = &result["process_info"];
    ensure!(
        info["foreground_process_group_id"] == process["child_pid"]
            && info["foreground_processes"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p["pid"] == process["child_pid"])),
        "owned native process is not the observed foreground receiver"
    );
    Ok(())
}

fn recovery_scope(c: &Connection, node: bool, id: &str) -> Result<String> {
    let sql = if node {
        "SELECT json_array(j.epoch,j.state,j.doc,j.checkpoint,j.command,c.record,c.content_hash,a.owner,a.node,a.key,a.epoch,p.epoch,p.state,p.doc) FROM jobs j JOIN commands c ON c.id=j.command JOIN authority a ON a.singleton=1 LEFT JOIN runtime_processes p ON p.attempt=j.id WHERE j.id=?1"
    } else {
        "SELECT json_array(l.generation,l.state,l.doc,p.generation,p.state,p.doc,r.state,json_extract(r.doc,'$.deadline'),EXISTS(SELECT 1 FROM principals x WHERE x.run=l.run AND x.role='supervisor' AND x.generation=l.generation AND x.active=1),EXISTS(SELECT 1 FROM coordinator_routes x WHERE x.run=l.run AND x.generation=l.generation)) FROM coordinator_launches l JOIN runs r ON r.id=l.run LEFT JOIN coordinator_processes p ON p.launch=l.id WHERE l.id=?1"
    };
    let raw: String = c.query_row(sql, [id], |r| r.get(0))?;
    Ok(super::model::digest(raw.as_bytes()))
}
