//! Renew transport authority around an immutable signed event. Application
//! replay requires positive controller evidence, never merely an old signature.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    identity::{Identity, MAX_WIRE_BYTES, Record, SignedRecord},
    model::{Request, digest, id, identifier},
    node::NodeStore,
    store::now,
};

pub(super) const TYPE: &str = "local.herdr.supervisor.eventRetry";
pub(super) const CONTROLLER_SCHEMA: &str = "
CREATE TABLE event_recoveries (
 owner TEXT NOT NULL, author TEXT NOT NULL, original_id TEXT NOT NULL,
 original_hash TEXT NOT NULL, accepted_record TEXT NOT NULL,
 PRIMARY KEY(owner,author,original_id),
 FOREIGN KEY(owner,author,accepted_record) REFERENCES inbox(owner,author,id)
) STRICT;
PRAGMA user_version=11;";
pub(super) const NODE_SCHEMA: &str = "
CREATE TABLE event_recoveries (
 operation TEXT PRIMARY KEY, original TEXT NOT NULL REFERENCES events(id),
 replacement TEXT NOT NULL UNIQUE REFERENCES events(id),
 original_hash TEXT NOT NULL, original_state TEXT NOT NULL, receipt TEXT NOT NULL
) STRICT;
CREATE INDEX event_recovery_original ON event_recoveries(original);
PRAGMA user_version=9;";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry {
    original: SignedRecord,
}

pub(super) fn original(signed: &SignedRecord) -> Result<Option<SignedRecord>> {
    if signed.record.record_type != TYPE {
        return Ok(None);
    }
    let retry: Retry = serde_json::from_value(signed.record.payload.clone())?;
    let r = &retry.original.record;
    r.encoded()?;
    retry.original.verify(r.created_at)?;
    ensure!(
        r.record_type == "local.herdr.supervisor.event"
            && r.owner == signed.record.owner
            && r.author == signed.record.author
            && r.recipient == signed.record.recipient
            && r.epoch == signed.record.epoch
            && r.id != signed.record.id
            && r.created_at <= now().saturating_add(30),
        "recovery must retain the original event's authority and epoch"
    );
    Ok(Some(retry.original))
}

pub(super) fn prior(c: &Connection, original: &SignedRecord) -> Result<bool> {
    let r = &original.record;
    let received: Option<String> = c
        .query_row(
            "SELECT content_hash FROM inbox WHERE owner=?1 AND author=?2 AND id=?3 AND applied=1",
            params![r.owner, r.author, r.id],
            |r| r.get(0),
        )
        .optional()?;
    let recovered: Option<String> = c.query_row("SELECT e.original_hash FROM event_recoveries e JOIN inbox i ON i.owner=e.owner AND i.author=e.author AND i.id=e.accepted_record AND i.applied=1 WHERE e.owner=?1 AND e.author=?2 AND e.original_id=?3",
        params![r.owner,r.author,r.id], |r|r.get(0)).optional()?;
    for hash in received.iter().chain(recovered.iter()) {
        ensure!(
            hash == &original.content_hash,
            "original event ID collision"
        );
    }
    Ok(received.is_some() || recovered.is_some())
}

pub(super) fn remember(
    tx: &Transaction<'_>,
    original: &SignedRecord,
    accepted: &str,
) -> Result<()> {
    let r = &original.record;
    tx.execute("INSERT INTO event_recoveries VALUES (?1,?2,?3,?4,?5) ON CONFLICT(owner,author,original_id) DO NOTHING",
        params![r.owner,r.author,r.id,original.content_hash,accepted])?;
    Ok(())
}

/// Only call after a matching accepted inbox/recovery record has been proved.
pub(super) fn accepted_response(c: &Connection, request: &Request) -> Result<Value> {
    let (hash,response):(String,String)=c.query_row("SELECT o.request_hash,o.response FROM operations o JOIN principals p ON p.id=o.principal WHERE p.token_hash=?1 AND o.id=?2",
        params![digest(request.token.as_bytes()),request.operation_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        hash == digest(&serde_json::to_vec(&request.command)?),
        "accepted operation content changed"
    );
    Ok(serde_json::from_str(&response)?)
}

impl NodeStore {
    pub fn recover_event(
        &mut self,
        event: &str,
        operation: &str,
        identity: &Identity,
    ) -> Result<Value> {
        uuid::Uuid::parse_str(event)?;
        identifier(operation)?;
        let _lease = super::api::lock(&self.dir, "execution.lock")?;
        self.grant.verify(now())?;
        ensure!(
            identity.did() == self.grant.record.recipient,
            "wrong recovery signing key"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (local,hash,wire,state):(String,String,String,String)=tx.query_row("SELECT id,request_hash,wire,state FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.id')=?1",[event],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        ensure!(
            wire.len() <= MAX_WIRE_BYTES,
            "stored event exceeds wire limit"
        );
        let original: SignedRecord = serde_json::from_str(&wire)?;
        let r = &original.record;
        original.verify(r.created_at)?;
        ensure!(
            r.record_type == "local.herdr.supervisor.event"
                && r.owner == self.grant.record.owner
                && r.recipient == r.owner
                && r.author == identity.did()
                && r.epoch == self.grant.record.epoch
                && r.created_at <= now().saturating_add(30),
            "event recovery authority changed"
        );
        let request: Request = serde_json::from_value(r.payload.clone())?;
        ensure!(
            hash == digest(&serde_json::to_vec(&request)?)
                && local
                    == format!(
                        "{}:{}",
                        digest(request.token.as_bytes()),
                        request.operation_id
                    ),
            "stored event fingerprint changed"
        );
        let previous: Option<(String, String, String)> = tx
            .query_row(
                "SELECT original,original_hash,receipt FROM event_recoveries WHERE operation=?1",
                [operation],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((prior, hash, receipt)) = previous {
            ensure!(
                prior == local && hash == original.content_hash,
                "recovery operation content changed"
            );
            return Ok(serde_json::from_str(&receipt)?);
        }
        ensure!(
            ["pending", "broker_stored", "expired"].contains(&state.as_str())
                && r.expires_at <= now(),
            "only an expired event without an application receipt can be recovered"
        );
        let count: u64 = tx.query_row(
            "SELECT count(*) FROM event_recoveries WHERE original=?1",
            [&local],
            |r| r.get(0),
        )?;
        ensure!(count < 32, "event recovery history limit reached");
        let inflight:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM event_recoveries r JOIN events e ON e.id=r.replacement WHERE r.original=?1 AND e.state IN ('pending','broker_stored') AND json_extract(e.wire,'$.record.expires_at')>?2)",params![local,now()],|r|r.get(0))?;
        ensure!(
            !inflight,
            "recovery is still in progress; reuse its operation ID"
        );
        let replacement_id = id();
        let replacement = identity.sign(Record {
            record_type: TYPE.into(),
            id: replacement_id.clone(),
            owner: r.owner.clone(),
            author: r.author.clone(),
            recipient: r.recipient.clone(),
            epoch: r.epoch,
            created_at: now(),
            expires_at: self.grant.record.expires_at,
            payload: serde_json::to_value(Retry {
                original: original.clone(),
            })?,
        })?;
        let bytes = serde_json::to_string(&replacement)?;
        ensure!(bytes.len() <= MAX_WIRE_BYTES, "recovery exceeds wire limit");
        let replacement_local = format!("recovery:{replacement_id}");
        tx.execute(
            "INSERT INTO events(id,request_hash,wire,state) VALUES (?1,?2,?3,'pending')",
            params![replacement_local, hash, bytes],
        )?;
        let receipt = json!({"operation":operation,"original_event":event,"recovery_event":replacement_id,
            "state":"queued","controller_applied":false,"expires_at":replacement.record.expires_at});
        tx.execute(
            "INSERT INTO event_recoveries VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                operation,
                local,
                replacement_local,
                original.content_hash,
                state,
                serde_json::to_string(&receipt)?
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }
}

/// A fresh controller receipt may confirm the original logical operation via a
/// replacement delivery. Keep both signed wires and expose the recovery link.
pub(super) fn receive_receipt(tx: &Transaction<'_>, body: &Value) -> Result<()> {
    let event = body["event_id"].as_str().context("receipt event ID")?;
    let operation = body["operation_id"]
        .as_str()
        .context("receipt operation ID")?;
    let result = body.get("result").context("receipt result missing")?;
    let (local,wire):(String,String)=tx.query_row("SELECT id,wire FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.id')=?1",[event],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let signed: SignedRecord = serde_json::from_str(&wire)?;
    let original = original(&signed)?;
    let request: Request =
        serde_json::from_value(original.as_ref().unwrap_or(&signed).record.payload.clone())?;
    ensure!(
        request.operation_id == operation,
        "receipt operation mismatch"
    );
    let root = if let Some(original) = &original {
        let (root, hash): (String, String) = tx.query_row(
            "SELECT original,original_hash FROM event_recoveries WHERE replacement=?1",
            [&local],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (fingerprint, wire): (String, String) = tx.query_row(
            "SELECT request_hash,wire FROM events WHERE id=?1",
            [&root],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            fingerprint == digest(&serde_json::to_vec(&request)?)
                && hash == original.content_hash
                && serde_json::to_value(serde_json::from_str::<SignedRecord>(&wire)?)?
                    == serde_json::to_value(original)?,
            "recovery no longer matches original event"
        );
        root
    } else {
        local.clone()
    };
    for target in [&local, &root] {
        let previous: Option<String> =
            tx.query_row("SELECT response FROM events WHERE id=?1", [target], |r| {
                r.get(0)
            })?;
        if let Some(previous) = previous {
            ensure!(
                serde_json::from_str::<Value>(&previous)? == *result,
                "conflicting application receipt"
            );
        }
        tx.execute("UPDATE events SET state='controller_applied',response=?2,last_error='',available_at=0 WHERE id=?1",params![target,serde_json::to_string(result)?])?;
    }
    tx.execute("UPDATE events SET state='superseded',available_at=0,last_error='logical operation accepted through another delivery' WHERE state='pending' AND id IN (SELECT replacement FROM event_recoveries WHERE original=?1) AND id<>?2",params![root,local])?;
    Ok(())
}

pub(super) fn summary(c: &Connection, event: &str) -> Result<Value> {
    let mut q=c.prepare("SELECT json_extract(e.receipt,'$.recovery_event'),r.state FROM event_recoveries e JOIN events o ON o.id=e.original JOIN events r ON r.id=e.replacement WHERE json_extract(CASE WHEN json_valid(o.wire) THEN o.wire ELSE '{}' END,'$.record.id')=?1 ORDER BY e.rowid LIMIT 32")?;
    let items = q
        .query_map([event], |r| {
            Ok(json!({"event":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?}))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!(items))
}

pub(super) fn status(state: &std::path::Path, event: &str) -> Result<Value> {
    uuid::Uuid::parse_str(event)?;
    let mut c = super::reception::local_db(state, true, false)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Deferred)?;
    ensure!(
        tx.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))? == 10,
        "node service migration required"
    );
    let (state,tries,available,error):(String,u64,i64,String)=tx.query_row("SELECT state,tries,available_at,last_error FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.id')=?1",[event],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    let result = json!({"event":event,"state":state,"controller_applied":state=="controller_applied","tries":tries,"next_attempt_at":available,"last_error":error,"recoveries":summary(&tx,event)?});
    tx.commit()?;
    Ok(result)
}
