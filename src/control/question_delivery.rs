//! Native hints for durable scoped answers; a send receipt is not model consumption.
use super::{
    model::digest,
    node::NodeStore,
    runtime::{LaunchPlan, RuntimeConfig},
    store::now,
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

/// The same execution lease fences cancellation, registration and this send.
/// Freeze the recipient/body/token identity before calling mailer; replay uses
/// its stable operation key. Never equate the message receipt with model use.
pub(super) fn deliver(store: &mut NodeStore, config: &RuntimeConfig) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "execution.lock")?;
    store.grant.verify(now())?;
    store.connection.execute("UPDATE question_notices SET state='retired' WHERE state='pending' AND EXISTS (SELECT 1 FROM jobs j WHERE j.id=question_notices.attempt AND (j.epoch<>question_notices.epoch OR j.state IN ('cancelling','stopped')))",[])?;
    store.connection.execute("UPDATE question_notices SET state='retired' WHERE state='pending' AND EXISTS (SELECT 1 FROM jobs j JOIN commands c ON c.id=j.command WHERE j.id=question_notices.attempt AND json_extract(c.record,'$.record.expires_at')<=?1)",[now()])?;
    // One bounded IPC per service cycle. Retry counts let other attempts make
    // progress when one receiver is unavailable, without changing send identity.
    type Pending = (String, u64, String, Option<String>, Option<String>, String);
    let rows: Vec<Pending> = {
        let mut q=store.connection.prepare("SELECT m.attempt,m.epoch,m.question,m.prepared,m.token_hash,j.checkpoint FROM question_notices m JOIN jobs j ON j.id=m.attempt WHERE m.state='pending' AND j.state='bootstrapped' ORDER BY m.tries,m.rowid LIMIT 1")?;
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
    for (attempt, epoch, question, prepared, token_hash, checkpoint) in rows {
        store.connection.execute(
            "UPDATE question_notices SET tries=tries+1 WHERE attempt=?1 AND epoch=?2 AND question=?3",
            params![attempt, epoch, question],
        )?;
        let plan: LaunchPlan = serde_json::from_str(&checkpoint)?;
        ensure!(
            &plan.config == config,
            "question runtime configuration changed"
        );
        let receiver = plan
            .registration
            .as_ref()
            .context("question recipient registration")?;
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
                "question sender credential changed"
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
                "operationId":format!("question-{attempt}-{epoch}-{question}"),
                "body":format!("Task question {question} has a supervisor answer. Read the questions field with: {prefix} context . Check the current assignment before acting. This answer is context, not a contract amendment or integration permission.")});
            store.connection.execute("UPDATE question_notices SET prepared=?4,token_hash=?5 WHERE attempt=?1 AND epoch=?2 AND question=?3",params![attempt,epoch,question,serde_json::to_string(&request)?,hash])?;
            request
        };
        ensure!(
            request["recipient"] == receiver["agent"]
                && request["recipientSession"] == receiver["id"]
                && request["recipientEpoch"] == receiver["epoch"],
            "question recipient changed"
        );
        let mut request = request;
        let capabilities = super::api::call_json(
            &config.mailer_state.join("daemon.sock"),
            &json!({"op":"capabilities","token":token}),
        )?;
        ensure!(
            capabilities["idempotentSend"] == true && capabilities["recipientPrecondition"] == true,
            "mailer protocol 2 required for question notices"
        );
        request["token"] = json!(token);
        let receipt = super::api::call_json(&config.mailer_state.join("daemon.sock"), &request)?;
        ensure!(
            receipt["id"].as_str().is_some_and(|s| !s.is_empty())
                && receipt["recipientSession"] == receiver["id"]
                && receipt["recipientEpoch"] == receiver["epoch"],
            "mailer question receipt scope mismatch"
        );
        store.connection.execute("UPDATE question_notices SET state='submitted',receipt=?4 WHERE attempt=?1 AND epoch=?2 AND question=?3",params![attempt,epoch,question,serde_json::to_string(&receipt)?])?;
        delivered += 1;
    }
    Ok(json!({"submitted":delivered,"model_behavior_verified":false}))
}
