//! Node-local durable inbox and outgoing events. Broker receipt never starts a model.
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::identity::{Identity, Record, SignedRecord};
use super::model::{Attempt, Profile, Request, digest, identifier};
use super::store::{private_dir, secret_file};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteOrder {
    pub attempt: Attempt,
    pub contract: super::model::Contract,
    pub profile: Profile,
    pub repo: String,
    pub credential: String,
    pub memory: Value,
}

pub struct NodeStore {
    pub connection: Connection,
    pub dir: PathBuf,
    pub node_id: String,
    pub grant: SignedRecord,
}

impl NodeStore {
    /// Enrollment pins owner, node and epoch. Reopening never silently changes authority.
    pub fn open(
        dir: &Path,
        node_id: &str,
        owner: &str,
        grant: SignedRecord,
        identity: &Identity,
    ) -> Result<Self> {
        identifier(node_id)?;
        grant.verify(jiff::Timestamp::now().as_second())?;
        ensure!(
            grant.record.record_type == "local.herdr.supervisor.nodeGrant"
                && grant.record.owner == owner
                && grant.record.author == owner
                && grant.record.recipient == identity.did(),
            "invalid node delegation"
        );
        ensure!(
            grant.record.payload == json!({"node":node_id,"key":identity.did()})
                && grant.record.epoch > 0,
            "wrong node delegation scope"
        );
        private_dir(dir)?;
        let db = dir.join("node.sqlite");
        if !db.exists() {
            secret_file(&db, b"")?;
        }
        ensure!(
            std::fs::symlink_metadata(&db)?.is_file(),
            "node DB is not regular"
        );
        let mut connection = Connection::open(db)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                tx.execute_batch("CREATE TABLE authority (singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner TEXT NOT NULL, node TEXT NOT NULL, key TEXT NOT NULL, epoch INTEGER NOT NULL) STRICT;
                    CREATE TABLE commands (id TEXT PRIMARY KEY, content_hash TEXT NOT NULL, record TEXT NOT NULL, state TEXT NOT NULL, received INTEGER NOT NULL) STRICT;
                    CREATE TABLE jobs (id TEXT PRIMARY KEY, epoch INTEGER NOT NULL, state TEXT NOT NULL, command TEXT NOT NULL REFERENCES commands(id), doc TEXT NOT NULL, checkpoint TEXT NOT NULL DEFAULT '{}') STRICT;
                    CREATE TABLE events (id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, wire TEXT NOT NULL, state TEXT NOT NULL) STRICT;
                    PRAGMA user_version=2;")?;
                tx.execute_batch("ALTER TABLE events ADD COLUMN response TEXT;")?;
                tx.execute(
                    "INSERT INTO authority VALUES (1,?1,?2,?3,?4)",
                    params![owner, node_id, identity.did(), grant.record.epoch],
                )?;
            }
            1 => {
                tx.execute_batch(
                    "ALTER TABLE events ADD COLUMN response TEXT; PRAGMA user_version=2;",
                )?;
            }
            2 => (),
            _ => bail!("unsupported node schema"),
        }
        let existing: (String, String, String, u64) = tx.query_row(
            "SELECT owner,node,key,epoch FROM authority WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        ensure!(
            existing
                == (
                    owner.into(),
                    node_id.into(),
                    identity.did(),
                    grant.record.epoch
                ),
            "node authority changed; explicit migration required"
        );
        tx.commit()?;
        Ok(Self {
            connection,
            dir: dir.into(),
            node_id: node_id.into(),
            grant,
        })
    }

    pub fn receive(&mut self, signed: &SignedRecord) -> Result<()> {
        // Cancellation and the final resource mutation share this lease. A busy
        // executor causes broker redelivery, not an ACK before applying the fence.
        let _lease = super::api::lock(&self.dir, "execution.lock")?;
        let now = jiff::Timestamp::now().as_second();
        self.grant.verify(now)?;
        signed.verify(now)?;
        let r = &signed.record;
        ensure!(
            r.record_type == "local.herdr.supervisor.command"
                && r.author == self.grant.record.owner
                && r.owner == r.author
                && r.recipient == self.grant.record.recipient
                && r.epoch == self.grant.record.epoch,
            "command owner, route or epoch mismatch"
        );
        let kind = r.payload["kind"].as_str().context("command kind")?;
        let body = r.payload.get("body").context("command body")?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT content_hash FROM commands WHERE id=?1",
                [&r.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(hash) = old {
            ensure!(hash == signed.content_hash, "command ID content collision");
            return Ok(());
        }
        tx.execute(
            "INSERT INTO commands VALUES (?1,?2,?3,'persisted',?4)",
            params![
                r.id,
                signed.content_hash,
                serde_json::to_string(signed)?,
                now
            ],
        )?;
        match kind {
            "receipt" => {
                let event = body["event_id"].as_str().context("receipt event ID")?;
                let operation = body["operation_id"]
                    .as_str()
                    .context("receipt operation ID")?;
                let changed=tx.execute("UPDATE events SET state='controller_applied',response=?3 WHERE json_extract(wire,'$.record.id')=?1 AND json_extract(wire,'$.record.payload.operation_id')=?2",params![event,operation,serde_json::to_string(&body["result"])?])?;
                ensure!(changed == 1, "receipt references an unknown event");
            }
            "execute" => {
                let order: ExecuteOrder = serde_json::from_value(body.clone())?;
                ensure!(
                    order.attempt.node == self.node_id && order.attempt.epoch > 0,
                    "foreign attempt"
                );
                ensure!(
                    order.attempt.contract_hash == digest(&serde_json::to_vec(&order.contract)?),
                    "contract content mismatch"
                );
                let old: Option<(u64, String)> = tx
                    .query_row(
                        "SELECT epoch,state FROM jobs WHERE id=?1",
                        [&order.attempt.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                match old {
                    Some((epoch, state))
                        if epoch > order.attempt.epoch && state == "cancelling" =>
                    {
                        // A cancellation tombstone prevents delayed execution after a partition.
                        tx.execute(
                            "UPDATE commands SET state='superseded' WHERE id=?1",
                            [&r.id],
                        )?;
                    }
                    Some(_) => bail!("attempt already has an execution intent"),
                    None => {
                        tx.execute("INSERT INTO jobs(id,epoch,state,command,doc) VALUES (?1,?2,'reserved',?3,?4)",params![order.attempt.id,order.attempt.epoch,r.id,serde_json::to_string(&order)?])?;
                    }
                }
            }
            "cancel" => {
                let attempt: Attempt =
                    serde_json::from_value(body.get("attempt").context("cancel attempt")?.clone())?;
                ensure!(
                    attempt.node == self.node_id && attempt.state == "cancelling",
                    "foreign cancellation"
                );
                let current: Option<u64> = tx
                    .query_row("SELECT epoch FROM jobs WHERE id=?1", [&attempt.id], |r| {
                        r.get(0)
                    })
                    .optional()?;
                ensure!(
                    current.is_none_or(|epoch| attempt.epoch > epoch),
                    "stale cancellation epoch"
                );
                tx.execute("INSERT INTO jobs(id,epoch,state,command,doc) VALUES (?1,?2,'cancelling',?3,?4) ON CONFLICT(id) DO UPDATE SET epoch=excluded.epoch,state='cancelling'",params![attempt.id,attempt.epoch,r.id,serde_json::to_string(body)?])?;
            }
            _ => bail!("unsupported node command"),
        }
        tx.commit()?;
        Ok(())
    }

    pub fn enqueue(&mut self, request: &Request, identity: &Identity) -> Result<String> {
        ensure!(
            identity.did() == self.grant.record.recipient,
            "wrong node signing key"
        );
        let now = jiff::Timestamp::now().as_second();
        self.grant.verify(now)?;
        let fingerprint = digest(&serde_json::to_vec(request)?);
        let event_id = format!(
            "{}:{}",
            digest(request.token.as_bytes()),
            request.operation_id
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT request_hash,wire FROM events WHERE id=?1",
                [&event_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, wire)) = old {
            ensure!(hash == fingerprint, "event operation content collision");
            return Ok(serde_json::from_str::<SignedRecord>(&wire)?.record.id);
        }
        let record = identity.sign(Record {
            record_type: "local.herdr.supervisor.event".into(),
            id: super::model::id(),
            owner: self.grant.record.owner.clone(),
            author: identity.did(),
            recipient: self.grant.record.owner.clone(),
            epoch: self.grant.record.epoch,
            created_at: now,
            expires_at: self.grant.record.expires_at,
            payload: serde_json::to_value(request)?,
        })?;
        tx.execute(
            "INSERT INTO events(id,request_hash,wire,state) VALUES (?1,?2,?3,'pending')",
            params![event_id, fingerprint, serde_json::to_string(&record)?],
        )?;
        tx.commit()?;
        Ok(record.record.id)
    }

    pub fn pending(&self) -> Result<Vec<(String, SignedRecord)>> {
        let mut q = self
            .connection
            .prepare("SELECT id,wire FROM events WHERE state='pending' ORDER BY rowid LIMIT 100")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (id, wire) = r?;
            Ok((id, serde_json::from_str(&wire)?))
        })
        .collect()
    }

    pub fn published(&self, id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE events SET state='broker_stored' WHERE id=?1 AND state='pending'",
            [id],
        )?;
        Ok(())
    }
}
