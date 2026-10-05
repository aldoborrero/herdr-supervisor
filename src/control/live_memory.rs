//! Selected, versioned memory replication and native hints for running workers.
//! Assignment snapshots remain immutable; delivery never implies model adoption.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    model::{Attempt, Run, Task, digest},
    node::{ExecuteOrder, NodeStore},
    runtime::{LaunchPlan, RuntimeConfig},
    store::{document, effect, memory_snapshot, now},
};

pub(super) const SCHEMA: &str = "
CREATE TABLE worker_memory (
 attempt TEXT PRIMARY KEY REFERENCES jobs(id), epoch INTEGER NOT NULL,
 version INTEGER NOT NULL, snapshot TEXT NOT NULL, acknowledged INTEGER
) STRICT;
CREATE TABLE memory_notices (
 attempt TEXT NOT NULL REFERENCES jobs(id), epoch INTEGER NOT NULL, version INTEGER NOT NULL,
 state TEXT NOT NULL, prepared TEXT, token_hash TEXT, receipt TEXT,
 tries INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(attempt,epoch,version)
) STRICT;
PRAGMA user_version=4;";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    pub attempt: String,
    pub epoch: u64,
    pub run: String,
    pub version: u64,
    pub snapshot: Value,
}

pub(super) fn version(c: &Connection, run: &str) -> Result<u64> {
    Ok(c.query_row(
        "SELECT COUNT(*) FROM memory_revisions WHERE run=?1",
        [run],
        |r| r.get(0),
    )?)
}

/// Called inside the approval/withdrawal transaction, after the revision insert.
pub(super) fn publish(tx: &Transaction<'_>, run: &str, topic: &str) -> Result<()> {
    let r: Run = document(tx, "runs", run)?;
    if !["active", "paused"].contains(&r.state.as_str()) || r.deadline <= now() {
        return Ok(());
    }
    let revision = version(tx, run)?;
    let mut q = tx.prepare(
        "SELECT doc FROM attempts WHERE run=?1 AND active=1 AND state<>'cancelling' ORDER BY rowid",
    )?;
    let attempts = q
        .query_map([run], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for doc in attempts {
        let a: Attempt = serde_json::from_str(&doc)?;
        let task: Task = document(tx, "tasks", &a.task)?;
        if topic != "preferences" && !task.contract.memory_topics.iter().any(|t| t == topic) {
            continue;
        }
        let update = Update {
            attempt: a.id,
            epoch: a.epoch,
            run: run.into(),
            version: revision,
            snapshot: memory_snapshot(tx, run, &task.contract.memory_topics, 32000)?,
        };
        effect(
            tx,
            &a.node,
            "memory_update",
            &serde_json::to_value(update)?,
            r.deadline,
        )?;
    }
    Ok(())
}

fn validate(snapshot: &Value, order: &ExecuteOrder) -> Result<()> {
    let allowed = |topic: &str| {
        topic == "preferences" || order.contract.memory_topics.iter().any(|t| t == topic)
    };
    let topics = snapshot["topics"].as_object().context("memory topics")?;
    let omitted = snapshot["omitted"]
        .as_array()
        .context("omitted memory topics")?;
    let mut bytes = 0usize;
    for (topic, item) in topics {
        ensure!(allowed(topic), "memory topic outside assignment");
        let text = item["text"].as_str().context("memory text")?;
        bytes = bytes
            .checked_add(text.len())
            .context("memory budget overflow")?;
        ensure!(
            bytes <= 32000
                && item["revision"].as_u64().is_some_and(|r| r > 0)
                && item["hash"] == digest(text.as_bytes()),
            "invalid memory text, revision or hash"
        );
    }
    for topic in omitted {
        ensure!(
            topic.as_str().is_some_and(allowed),
            "omitted topic outside assignment"
        );
    }
    ensure!(
        snapshot["bytes"].as_u64() == Some(bytes as u64),
        "memory byte count mismatch"
    );
    Ok(())
}

pub(super) fn initialize(tx: &Transaction<'_>, order: &ExecuteOrder) -> Result<()> {
    validate(&order.memory, order)?;
    tx.execute(
        "INSERT INTO worker_memory(attempt,epoch,version,snapshot) VALUES (?1,?2,?3,?4)",
        params![
            order.attempt.id,
            order.attempt.epoch,
            order.memory_version,
            serde_json::to_string(&order.memory)?
        ],
    )?;
    Ok(())
}

pub(super) fn receive(tx: &Transaction<'_>, body: &Value) -> Result<()> {
    let update: Update = serde_json::from_value(body.clone())?;
    let (epoch, state, doc): (u64, String, String) = tx.query_row(
        "SELECT epoch,state,doc FROM jobs WHERE id=?1",
        [&update.attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    // A delayed update may be acknowledged as obsolete, but never injected into
    // an old process or used to resurrect a cancellation tombstone.
    ensure!(update.epoch <= epoch, "memory epoch is ahead of assignment");
    if update.epoch < epoch || ["cancelling", "stopped"].contains(&state.as_str()) {
        return Ok(());
    }
    let order: ExecuteOrder = serde_json::from_str(&doc)?;
    ensure!(
        update.epoch == epoch && update.run == order.attempt.run && update.version > 0,
        "memory run or epoch mismatch"
    );
    validate(&update.snapshot, &order)?;
    let old: Option<(u64, Value)> = tx
        .query_row(
            "SELECT version,snapshot FROM worker_memory WHERE attempt=?1",
            [&update.attempt],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(v, s)| Ok::<_, anyhow::Error>((v, serde_json::from_str(&s)?)))
        .transpose()?;
    let (version, snapshot) = old.unwrap_or((order.memory_version, order.memory));
    if update.version <= version {
        ensure!(
            update.version != version || update.snapshot == snapshot,
            "memory version content collision"
        );
        return Ok(());
    }
    tx.execute("INSERT INTO worker_memory(attempt,epoch,version,snapshot) VALUES (?1,?2,?3,?4) ON CONFLICT(attempt) DO UPDATE SET version=excluded.version,snapshot=excluded.snapshot",
        params![update.attempt,epoch,update.version,serde_json::to_string(&update.snapshot)?])?;
    // Keep prepared sends frozen. Only an untouched older hint is superseded.
    tx.execute("UPDATE memory_notices SET state='superseded' WHERE attempt=?1 AND state='pending' AND prepared IS NULL",[&update.attempt])?;
    tx.execute(
        "INSERT INTO memory_notices(attempt,epoch,version,state) VALUES (?1,?2,?3,'pending')",
        params![update.attempt, epoch, update.version],
    )?;
    Ok(())
}

pub(super) fn context(c: &Connection, order: &ExecuteOrder) -> Result<Value> {
    let row: Option<(u64, String, Option<u64>)> = c
        .query_row(
            "SELECT version,snapshot,acknowledged FROM worker_memory WHERE attempt=?1 AND epoch=?2",
            params![order.attempt.id, order.attempt.epoch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (version, snapshot, acknowledged) = match row {
        Some((v, s, a)) => (v, serde_json::from_str::<Value>(&s)?, a),
        None => (order.memory_version, order.memory.clone(), None),
    };
    Ok(json!({"version":version,"snapshot":snapshot,"acknowledged_version":acknowledged}))
}

pub(super) fn acknowledge(c: &Connection, order: &ExecuteOrder, version: u64) -> Result<Value> {
    let changed = c.execute(
        "UPDATE worker_memory SET acknowledged=?3 WHERE attempt=?1 AND epoch=?2 AND version=?3",
        params![order.attempt.id, order.attempt.epoch, version],
    )?;
    ensure!(
        changed == 1,
        "memory changed; read current context before acknowledging"
    );
    Ok(
        json!({"attempt":order.attempt.id,"version":version,"acknowledged":true,"model_behavior_verified":false}),
    )
}

/// The same execution lease fences cancellation, registration and this send.
/// Freeze the recipient/body/token identity before calling mailer; replay uses
/// its stable operation key. Never equate the message receipt with model use.
pub(super) fn deliver(store: &mut NodeStore, config: &RuntimeConfig) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "execution.lock")?;
    store.grant.verify(now())?;
    store.connection.execute("UPDATE memory_notices SET state='retired' WHERE state='pending' AND EXISTS (SELECT 1 FROM jobs j WHERE j.id=memory_notices.attempt AND (j.epoch<>memory_notices.epoch OR j.state IN ('cancelling','stopped')))",[])?;
    store.connection.execute("UPDATE memory_notices SET state='retired' WHERE state='pending' AND EXISTS (SELECT 1 FROM jobs j JOIN commands c ON c.id=j.command WHERE j.id=memory_notices.attempt AND json_extract(c.record,'$.record.expires_at')<=?1)",[now()])?;
    // One bounded IPC per service cycle. Retry counts let other attempts make
    // progress when one receiver is unavailable, without changing send identity.
    type Pending = (String, u64, u64, Option<String>, Option<String>, String);
    let rows: Vec<Pending> = {
        let mut q=store.connection.prepare("SELECT m.attempt,m.epoch,m.version,m.prepared,m.token_hash,j.checkpoint FROM memory_notices m JOIN jobs j ON j.id=m.attempt WHERE m.state='pending' AND j.state='bootstrapped' ORDER BY m.tries,m.rowid LIMIT 1")?;
        q.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<std::result::Result<_, _>>()?
    };
    let mut delivered = 0;
    for (attempt, epoch, version, prepared, token_hash, checkpoint) in rows {
        store.connection.execute(
            "UPDATE memory_notices SET tries=tries+1 WHERE attempt=?1 AND epoch=?2 AND version=?3",
            params![attempt, epoch, version],
        )?;
        let plan: LaunchPlan = serde_json::from_str(&checkpoint)?;
        ensure!(
            &plan.config == config,
            "memory runtime configuration changed"
        );
        let receiver = plan
            .registration
            .as_ref()
            .context("memory recipient registration")?;
        let token = super::runtime::credential(
            &store
                .dir
                .join("workers")
                .join(&attempt)
                .join("mailer.token"),
        )?;
        let hash = digest(token.as_bytes());
        let request = if let Some(prepared) = prepared {
            ensure!(
                token_hash.as_deref() == Some(hash.as_str()),
                "memory sender credential changed"
            );
            serde_json::from_str::<Value>(&prepared)?
        } else {
            let prefix = format!(
                "{} control --state {} worker --credential {} --attempt {} --epoch {}",
                crate::remote::quote(config.supervisor_binary.to_str().context("binary path")?),
                crate::remote::quote(store.dir.to_str().context("node path")?),
                crate::remote::quote(
                    store
                        .dir
                        .join("workers")
                        .join(&attempt)
                        .join("worker.token")
                        .to_str()
                        .context("token path")?
                ),
                attempt,
                epoch
            );
            let request = json!({"op":"send","recipient":receiver["agent"],"recipientSession":receiver["id"],"recipientEpoch":receiver["epoch"],
                "operationId":format!("memory-{attempt}-{epoch}-{version}"),
                "body":format!("Approved run memory changed (version {version}). Read the current_memory field with: {prefix} context . It supersedes the startup memory snapshot and may withdraw old topics. After reading the latest version, report it with: {prefix} acknowledge-memory VERSION . This notice is not a new task or permission to change your contract.")});
            store.connection.execute("UPDATE memory_notices SET prepared=?4,token_hash=?5 WHERE attempt=?1 AND epoch=?2 AND version=?3",params![attempt,epoch,version,serde_json::to_string(&request)?,hash])?;
            request
        };
        ensure!(
            request["recipient"] == receiver["agent"]
                && request["recipientSession"] == receiver["id"]
                && request["recipientEpoch"] == receiver["epoch"],
            "memory recipient changed"
        );
        let mut request = request;
        let capabilities = super::api::call_json(
            &config.mailer_state.join("daemon.sock"),
            &json!({"op":"capabilities","token":token}),
        )?;
        ensure!(
            capabilities["idempotentSend"] == true && capabilities["recipientPrecondition"] == true,
            "mailer protocol 2 required for memory notices"
        );
        request["token"] = json!(token);
        let receipt = super::api::call_json(&config.mailer_state.join("daemon.sock"), &request)?;
        ensure!(
            receipt["id"].as_str().is_some_and(|s| !s.is_empty())
                && receipt["recipientSession"] == receiver["id"]
                && receipt["recipientEpoch"] == receiver["epoch"],
            "mailer memory receipt scope mismatch"
        );
        store.connection.execute("UPDATE memory_notices SET state='submitted',receipt=?4 WHERE attempt=?1 AND epoch=?2 AND version=?3",params![attempt,epoch,version,serde_json::to_string(&receipt)?])?;
        delivered += 1;
    }
    Ok(json!({"submitted":delivered,"model_behavior_verified":false}))
}
