//! Explicit local maintenance for a lost registration BEFORE any native launch.
//! Reauthorization is transactional and replayable; it performs no remote effect.
use std::path::Path;

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
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let schema: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        schema == if node { 8 } else { 9 },
        "unsupported recovery schema; run the current service migration first"
    );
    if let Some(receipt) = recorded(&tx, id, operation, epoch)? {
        return Ok(receipt);
    }
    let response = json!({"id":id,"node":node,"epoch":epoch,"operation":operation,"recorded_state":"registration_retry_authorized","recorded_at":now(),"registration_attempted":false,"model_launched":false});
    if node {
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
            ["claude", "pi"].contains(&order.profile.runtime.as_str())
                && !p.launch_started
                && !p.wrapper_started
                && p.wrapper_pid.is_none()
                && p.bind_event.is_none()
                && p.bootstrap.is_none()
                && p.registration.is_none()
                && p.registration_started,
            "registration retry requires an unlaunched receiver with a lost response"
        );
        uuid::Uuid::parse_str(
            p.expected_session
                .as_deref()
                .context("native UUID missing")?,
        )?;
        let process: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_processes WHERE attempt=?1)",
            [id],
            |r| r.get(0),
        )?;
        ensure!(
            !process,
            "native process evidence forbids registration retry"
        );
        no_receiver_files(&state.join("workers").join(id))?;
        p.registration_started = false;
        tx.execute(
            "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
            params![id, serde_json::to_string(&p)?],
        )?;
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
            ["claude", "pi"].contains(&p.profile.runtime.as_str())
                && !p.launch_started
                && !p.wrapper_started
                && p.registration.is_none()
                && p.registration_started,
            "registration retry requires an unlaunched receiver with a lost response"
        );
        uuid::Uuid::parse_str(
            p.expected_session
                .as_deref()
                .context("native UUID missing")?,
        )?;
        uuid::Uuid::parse_str(&p.run)?;
        let process: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM coordinator_processes WHERE launch=?1)",
            [id],
            |r| r.get(0),
        )?;
        ensure!(
            !process,
            "native process evidence forbids registration retry"
        );
        no_receiver_files(&coordinator_launch::files(state, &p))?;
        p.registration_started = false;
        tx.execute(
            "UPDATE coordinator_launches SET doc=?2,last_error='' WHERE id=?1",
            params![id, serde_json::to_string(&p)?],
        )?;
        tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('local-maintenance',?1,?2,'registration_recovery_authorized',?3,?4)", params![format!("registration-{id}-{operation}"),p.run,serde_json::to_string(&response)?,now()])?;
    }
    tx.execute(
        "INSERT INTO registration_recoveries VALUES (?1,?2,?3,?4)",
        params![id, operation, epoch, serde_json::to_string(&response)?],
    )?;
    tx.commit()?;
    Ok(response)
}
