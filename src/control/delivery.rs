//! Durable scheduling for outgoing transport. A retry/expiry is never a task result.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use super::store::now;

pub const NODE_SCHEMA: &str = "
ALTER TABLE events ADD COLUMN tries INTEGER NOT NULL DEFAULT 0;
ALTER TABLE events ADD COLUMN available_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE events ADD COLUMN last_error TEXT NOT NULL DEFAULT '';
PRAGMA user_version=5;";

#[derive(Clone, Copy)]
pub enum Queue {
    Controller,
    Node,
}

impl Queue {
    fn table(self) -> &'static str {
        match self {
            Self::Controller => "outbox",
            Self::Node => "events",
        }
    }
}

/// Reserve the next retry before publishing. Even a crash or lost local receipt
/// leaves a bounded delay; the frozen wire and logical operation are unchanged.
pub fn begin(c: &Connection, queue: Queue, id: &str) -> Result<bool> {
    Ok(c.execute(&format!("UPDATE {} SET tries=min(tries+1,1000000),available_at=?2+CASE WHEN tries>=5 THEN 60 ELSE (1 << (tries+1)) END,last_error='broker receipt not recorded' WHERE id=?1 AND state='pending' AND available_at<=?2",queue.table()),params![id,now()])? == 1)
}

pub fn failed(c: &Connection, queue: Queue, id: &str, reason: &'static str) -> Result<()> {
    c.execute(
        &format!(
            "UPDATE {} SET last_error=?2,available_at=max(available_at,?3+CASE WHEN tries>=6 THEN 60 ELSE (1 << tries) END) WHERE id=?1 AND state='pending'",
            queue.table()
        ),
        params![id, reason, now()],
    )?;
    Ok(())
}

pub fn terminal(c: &Connection, queue: Queue, id: &str, expired: bool) -> Result<()> {
    let (state, reason) = if expired {
        (
            "expired",
            "delivery validity expired; application outcome is not established",
        )
    } else {
        (
            "invalid",
            "stored outgoing envelope is invalid; local inspection required",
        )
    };
    c.execute(
        &format!(
            "UPDATE {} SET state=?2,last_error=?3,available_at=0 WHERE id=?1 AND state='pending'",
            queue.table()
        ),
        params![id, state, reason],
    )?;
    Ok(())
}

pub fn controller_due(c: &Connection) -> Result<Vec<(String, String)>> {
    // Internal controller effects belong to their own executors, not this relay.
    c.execute("UPDATE outbox SET state='expired',available_at=0,last_error='delivery validity expired; application outcome is not established' WHERE target<>'controller' AND state='pending' AND expires_at<=?1",[now()])?;
    let mut q=c.prepare("SELECT o.id,o.target FROM outbox o JOIN nodes n ON n.id=o.target AND n.active=1 WHERE o.state='pending' AND o.available_at<=?1 AND o.expires_at>?1 ORDER BY o.available_at,o.rowid LIMIT 16")?;
    Ok(q.query_map([now()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}

pub fn node_due(c: &Connection) -> Result<Vec<(String, String)>> {
    let mut q=c.prepare("SELECT id,wire FROM events WHERE state='pending' AND available_at<=?1 ORDER BY available_at,rowid LIMIT 16")?;
    Ok(q.query_map([now()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}

/// Bounded operator projection: no command bodies, tokens or signed envelopes.
pub fn controller_status(c: &Connection, target: &str) -> Result<Value> {
    let counts: Value = {
        let mut q =
            c.prepare("SELECT state,count(*) FROM outbox WHERE target=?1 GROUP BY state")?;
        let mut result = serde_json::Map::new();
        for row in q.query_map([target], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?))
        })? {
            let (state, count) = row?;
            result.insert(state, json!(count));
        }
        Value::Object(result)
    };
    let mut q=c.prepare("SELECT id,kind,state,tries,available_at,expires_at,last_error FROM outbox WHERE target=?1 ORDER BY rowid DESC LIMIT 100")?;
    let rows=q.query_map([target],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"tries":r.get::<_,u64>(3)?,"next_attempt_at":r.get::<_,i64>(4)?,"expires_at":r.get::<_,i64>(5)?,"last_error":r.get::<_,String>(6)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(json!({"target":target,"state_counts":counts,"recent":rows,"limit":100}))
}
