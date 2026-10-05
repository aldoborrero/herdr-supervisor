//! Renew delivery of an existing cancellation, never its epoch or process result.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use super::{
    identity::SignedRecord,
    model::{Attempt, Mutation, Principal, digest},
    store::{document, effect, now, revision, supervise},
};

pub(super) fn renew(tx: &Transaction<'_>, p: &Principal, command: &Mutation) -> Result<Value> {
    let Mutation::RenewCancellation {
        attempt,
        expected_epoch,
        expired_command,
    } = command
    else {
        unreachable!()
    };
    uuid::Uuid::parse_str(expired_command)?;
    let a: Attempt = document(tx, "attempts", attempt)?;
    supervise(p, &a.run)?;
    revision(a.epoch, *expected_epoch)?;
    ensure!(
        a.state == "cancelling",
        "attempt is not awaiting cancellation"
    );
    let (target, kind, payload, wire, state, expires): (
        String,
        String,
        String,
        Option<String>,
        String,
        i64,
    ) = tx.query_row(
        "SELECT target,kind,payload,wire,state,expires_at FROM outbox WHERE id=?1",
        [expired_command],
        |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let body: Value = serde_json::from_str(&payload)?;
    ensure!(
        target == a.node
            && kind == "cancel"
            && body == json!({"attempt":a})
            && ["pending", "expired", "broker_stored"].contains(&state.as_str())
            && expires <= now(),
        "original must be an expired cancellation for this exact attempt and epoch"
    );
    let (key, epoch): (String, u64) = tx.query_row(
        "SELECT key,epoch FROM nodes WHERE id=?1 AND active=1",
        [&a.node],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if let Some(wire) = wire {
        let signed: SignedRecord = serde_json::from_str(&wire)?;
        let r = &signed.record;
        signed.verify(r.created_at)?;
        let owner: String =
            tx.query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?;
        ensure!(
            r.record_type == "local.herdr.supervisor.command"
                && r.owner == owner
                && r.author == owner
                && r.recipient == key
                && r.epoch == epoch
                && r.id == *expired_command
                && r.expires_at == expires
                && r.created_at <= now().saturating_add(30)
                && r.payload == json!({"kind":"cancel","body":body}),
            "original signed cancellation no longer matches authority or intent"
        );
    }
    let inflight:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE target=?1 AND kind='cancel' AND state IN ('pending','broker_stored') AND expires_at>?2 AND json_extract(payload,'$.attempt.id')=?3 AND json_extract(payload,'$.attempt.epoch')=?4)",params![a.node,now(),a.id,a.epoch],|r|r.get(0))?;
    ensure!(
        !inflight,
        "a current cancellation delivery already exists; replay its operation ID"
    );
    let deadline = now() + 86400;
    let delivery = effect(tx, &a.node, "cancel", &body, deadline)?;
    Ok(
        json!({"attempt":a.id,"epoch":a.epoch,"original_command":expired_command,
        "command":delivery,"body_hash":digest(&serde_json::to_vec(&body)?),"expires_at":deadline,
        "state":"queued","process_stop_confirmed":false,"capacity_released":false}),
    )
}

/// A renewed command has a fresh transport UUID. An equal epoch is idempotent
/// only with positive, identical cancellation history; a stopped job stays stopped.
pub(super) fn already_applied(
    tx: &Transaction<'_>,
    a: &Attempt,
    body: &Value,
    current: &str,
) -> Result<bool> {
    let job: Option<(u64, String)> = tx
        .query_row("SELECT epoch,state FROM jobs WHERE id=?1", [&a.id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((epoch, state)) = job else {
        return Ok(false);
    };
    if epoch != a.epoch {
        return Ok(false);
    }
    ensure!(
        ["cancelling", "stopped"].contains(&state.as_str()),
        "same epoch does not prove a previous cancellation"
    );
    let mut q=tx.prepare("SELECT record FROM commands WHERE id<>?1 AND json_extract(record,'$.record.payload.kind')='cancel' AND json_extract(record,'$.record.payload.body.attempt.id')=?2 AND json_extract(record,'$.record.payload.body.attempt.epoch')=?3")?;
    let mut rows = q.query(params![current, a.id, a.epoch])?;
    let mut proven = false;
    while let Some(row) = rows.next()? {
        let signed: SignedRecord = serde_json::from_str(&row.get::<_, String>(0)?)?;
        ensure!(
            signed.record.payload["body"] == *body,
            "same cancellation epoch has conflicting content"
        );
        proven = true;
    }
    ensure!(proven, "same epoch has no persisted cancellation evidence");
    Ok(true)
}
