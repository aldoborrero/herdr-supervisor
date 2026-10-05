//! Owner-controlled drain and application-authority revocation. Neither operation
//! signals a native process or rewrites an already signed broker record.
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};

use super::{
    identity::{Identity, SignedRecord},
    model::{Mutation, Principal},
    store::{Store, authenticate, own, revision},
};

pub(super) const SCHEMA: &str = "
ALTER TABLE nodes ADD COLUMN draining INTEGER NOT NULL DEFAULT 0 CHECK(draining IN (0,1));
ALTER TABLE nodes ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0);
PRAGMA user_version=10;";

#[derive(Serialize)]
pub(super) struct Node {
    id: String,
    key: String,
    epoch: u64,
    revision: u64,
    max_active: u32,
    active: bool,
    draining: bool,
    active_attempts: u64,
}

fn read(c: &Connection, node: &str) -> Result<Node> {
    super::broker_policy::node_id(node)?;
    Ok(c.query_row("SELECT id,key,epoch,revision,max_active,active,draining,(SELECT count(*) FROM attempts a WHERE a.node=n.id AND a.active=1) FROM nodes n WHERE id=?1", [node], |r| Ok(Node {
        id:r.get(0)?, key:r.get(1)?, epoch:r.get(2)?, revision:r.get(3)?, max_active:r.get(4)?, active:r.get(5)?, draining:r.get(6)?, active_attempts:r.get(7)?
    }))?)
}

impl Store {
    pub fn node_status(&self, token: &str, node: &str) -> Result<Value> {
        let tx = self.connection.unchecked_transaction()?;
        own(&authenticate(&tx, token)?)?;
        let result = serde_json::to_value(read(&tx, node)?)?;
        tx.commit()?;
        Ok(result)
    }

    /// Signing is owner-local, but must still match the current enrollment. An
    /// old key/epoch or retired node cannot receive a new usable delegation.
    pub fn node_grant(
        &mut self,
        node: &str,
        key: &str,
        epoch: u64,
        expires_at: i64,
    ) -> Result<SignedRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let owner = Identity::load(&self.dir.join("owner.key"))?;
        let pinned: String =
            tx.query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?;
        ensure!(owner.did() == pinned, "owner key does not match controller");
        let n = read(&tx, node)?;
        ensure!(
            n.active && n.key == key && n.epoch == epoch,
            "grant must match an active node key and epoch"
        );
        let grant = owner.delegation(node, key, epoch, expires_at)?;
        grant.verify(super::store::now())?;
        tx.commit()?;
        Ok(grant)
    }
}

pub(super) fn change(
    tx: &Transaction<'_>,
    principal: &Principal,
    command: &Mutation,
) -> Result<Value> {
    own(principal)?;
    let (node, expected_epoch, expected_revision, drain) = match command {
        Mutation::SetNodeDrain {
            node,
            expected_epoch,
            expected_revision,
            draining,
        } => (node, *expected_epoch, *expected_revision, Some(*draining)),
        Mutation::RevokeNode {
            node,
            expected_epoch,
            expected_revision,
        } => (node, *expected_epoch, *expected_revision, None),
        _ => unreachable!(),
    };
    let n = read(tx, node)?;
    ensure!(n.active, "node is revoked");
    ensure!(n.epoch == expected_epoch, "stale node epoch");
    revision(n.revision, expected_revision)?;
    ensure!(n.revision < i64::MAX as u64, "node revision exhausted");
    let retired = if let Some(draining) = drain {
        ensure!(
            n.draining != draining,
            "node already has the requested drain state"
        );
        tx.execute(
            "UPDATE nodes SET draining=?2,revision=revision+1 WHERE id=?1",
            params![node, draining],
        )?;
        0
    } else {
        ensure!(n.draining, "drain the node before revocation");
        ensure!(
            n.active_attempts == 0,
            "node still owns active or unresolved attempts"
        );
        ensure!(n.epoch < i64::MAX as u64, "node epoch exhausted");
        tx.execute(
            "UPDATE nodes SET active=0,epoch=epoch+1,revision=revision+1 WHERE id=?1",
            [node],
        )?;
        tx.execute(
            "UPDATE principals SET active=0 WHERE role='node' AND node=?1",
            [node],
        )?;
        // Retain payloads, signatures, retry metadata and old broker receipts.
        // A publication already in flight can still reach the broker; this local
        // state explicitly does not claim remote deletion or cancellation.
        tx.execute("UPDATE outbox SET state='node_revoked',available_at=0,last_error='node authority revoked; prior remote delivery remains possible' WHERE target=?1 AND state='pending'", [node])?
    };
    let mut result = serde_json::to_value(read(tx, node)?)?;
    result["retired_pending_effects"] = json!(retired);
    result["processes_signalled"] = json!(false);
    result["broker_credentials_revoked"] = json!(false);
    Ok(result)
}
