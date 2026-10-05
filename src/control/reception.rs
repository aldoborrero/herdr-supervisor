//! Rejected broker deliveries are durably held before TERM, never application ACKed.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

use super::{model::digest, store::now, transport::decode};

pub const SCHEMA: &str = "
CREATE TABLE reception_failures (
 id TEXT PRIMARY KEY, wire BLOB, bytes INTEGER NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('pending','invalid','expired','applied')),
 tries INTEGER NOT NULL, available_at INTEGER NOT NULL,
 reason TEXT NOT NULL, observed INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT;
CREATE INDEX reception_due ON reception_failures(state,available_at);
PRAGMA user_version=6;";
const MAX_ROWS: i64 = 4096;
const MAX_BYTES: i64 = 64 * 1024 * 1024;

fn classification(wire: &[u8]) -> (&'static str, &'static str) {
    let Ok(signed) = decode(wire) else {
        return ("invalid", "invalid envelope encoding or size");
    };
    // Test immutable integrity independently of the receiver's current clock.
    if signed.verify(signed.record.created_at).is_err() {
        return ("invalid", "invalid envelope integrity");
    }
    if signed.record.expires_at <= now() {
        return (
            "expired",
            "record validity expired; outcome is not established",
        );
    }
    ("pending", "application rejected; local retry scheduled")
}

pub fn known(c: &Connection, wire: &[u8]) -> Result<bool> {
    Ok(c.query_row(
        "SELECT 1 FROM reception_failures WHERE id=?1",
        [digest(wire)],
        |_| Ok(()),
    )
    .optional()?
    .is_some())
}

/// A full/unwritable store fails closed: the caller MUST NOT terminate delivery.
/// Oversized documents retain only their hash/size; they can never pass decode.
pub fn hold(c: &mut Connection, wire: &[u8]) -> Result<()> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if known(&tx, wire)? {
        return Ok(());
    }
    let (state, reason) = classification(wire);
    let saved = (wire.len() <= 300_000).then_some(wire);
    let (rows, bytes): (i64, i64) = tx.query_row(
        "SELECT count(*),COALESCE(sum(length(wire)),0) FROM reception_failures",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    ensure!(
        rows < MAX_ROWS && bytes + saved.map_or(0, |s| s.len() as i64) <= MAX_BYTES,
        "local rejected-delivery storage limit reached; broker delivery retained"
    );
    tx.execute(
        "INSERT INTO reception_failures VALUES (?1,?2,?3,?4,1,?5,?6,?7,?7)",
        params![
            digest(wire),
            saved,
            wire.len() as i64,
            state,
            if state == "pending" { now() + 2 } else { 0 },
            reason,
            now()
        ],
    )?;
    tx.commit()?;
    Ok(())
}

pub trait Receiver {
    fn db(&self) -> &Connection;
    fn accept(&mut self, wire: &[u8]) -> Result<()>;
}
impl Receiver for super::store::Store {
    fn db(&self) -> &Connection {
        &self.connection
    }
    fn accept(&mut self, wire: &[u8]) -> Result<()> {
        self.receive(&decode(wire)?).map(|_| ())
    }
}
impl Receiver for super::node::NodeStore {
    fn db(&self) -> &Connection {
        &self.connection
    }
    fn accept(&mut self, wire: &[u8]) -> Result<()> {
        self.receive(&decode(wire)?)
    }
}

/// Keep the original bytes and rerun ALL authorization/application checks.
/// A crash after application but before this receipt replays the existing operation.
pub fn retry(receiver: &mut impl Receiver) -> Result<usize> {
    let rows = {
        let mut q = receiver.db().prepare("SELECT id,wire FROM reception_failures WHERE state='pending' AND available_at<=?1 ORDER BY available_at,rowid LIMIT 16")?;
        q.query_map([now()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut count = 0;
    for (id, wire) in rows {
        // Reserve before application, including failures to write the final receipt.
        let reserved=receiver.db().execute("UPDATE reception_failures SET tries=min(tries+1,1000000),available_at=?2+CASE WHEN tries>=5 THEN 60 ELSE (1 << (tries+1)) END,updated=?2 WHERE id=?1 AND state='pending' AND available_at<=?2",params![id,now()])?;
        if reserved == 0 {
            continue;
        }
        let (mut state, mut reason) = if digest(&wire) == id {
            classification(&wire)
        } else {
            ("invalid", "stored bytes differ from received envelope")
        };
        if state == "pending" && receiver.accept(&wire).is_ok() {
            state = "applied";
            reason = "application committed; not a model receipt";
            count += 1;
        }
        receiver.db().execute("UPDATE reception_failures SET state=?2,reason=?3,available_at=CASE WHEN ?2='pending' THEN max(available_at,?4+CASE WHEN tries>=6 THEN 60 ELSE (1 << tries) END) ELSE 0 END,updated=?4 WHERE id=?1 AND state='pending'",params![id,state,reason,now()])?;
    }
    Ok(count)
}

/// Local operator view, deliberately excluding raw records, tokens and error text.
pub fn status(c: &Connection) -> Result<Value> {
    let mut counts = serde_json::Map::new();
    let mut q = c.prepare("SELECT state,count(*) FROM reception_failures GROUP BY state")?;
    for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (state, count) = row?;
        counts.insert(state, json!(count));
    }
    let mut q=c.prepare("SELECT id,bytes,state,tries,available_at,reason,observed,updated FROM reception_failures ORDER BY rowid DESC LIMIT 100")?;
    let rows=q.query_map([],|r|Ok(json!({"hash":r.get::<_,String>(0)?,"bytes":r.get::<_,u64>(1)?,"state":r.get::<_,String>(2)?,"tries":r.get::<_,u64>(3)?,"next_attempt_at":r.get::<_,i64>(4)?,"reason":r.get::<_,String>(5)?,"observed":r.get::<_,i64>(6)?,"updated":r.get::<_,i64>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(
        json!({"state_counts":counts,"recent":rows,"limit":100,"max_records":MAX_ROWS,"max_body_bytes":MAX_BYTES}),
    )
}

fn local_db(dir: &std::path::Path, node: bool, writable: bool) -> Result<Connection> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::symlink_metadata(dir)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "private state directory required"
    );
    let db = dir.join(if node {
        "node.sqlite"
    } else {
        "control.sqlite"
    });
    let metadata = std::fs::symlink_metadata(&db)?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "private regular database required"
    );
    let c = Connection::open_with_flags(
        db,
        if writable {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        },
    )?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    if writable {
        c.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    }
    Ok(c)
}

pub fn local_status(dir: &std::path::Path, node: bool) -> Result<Value> {
    status(&local_db(dir, node, false)?)
}

/// Explicit local maintenance only. Pending retries can never be pruned here.
pub fn prune(dir: &std::path::Path, node: bool, before: i64) -> Result<Value> {
    ensure!(
        before > 0 && before <= now(),
        "cutoff must be a past positive Unix timestamp"
    );
    let mut c = local_db(dir, node, true)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let removed=tx.execute("DELETE FROM reception_failures WHERE state IN ('invalid','expired','applied') AND updated<?1",[before])?;
    tx.commit()?;
    Ok(json!({"removed":removed,"before":before,"pending_preserved":true}))
}
