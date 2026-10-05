//! Local operator retirement of stopped reviewer copies. Retain chunk receipts;
//! other worktree files and Git history are outside this command's effect.
use super::{
    artifact_removal::{self, Identity},
    artifacts::{self, Candidate},
    cleanup,
    executor::{self, CheckoutPlan, ExecutorConfig},
    model::digest,
    node::ExecuteOrder,
    runtime::LaunchPlan,
    store::now,
};
use crate::{
    herdr::Herdr,
    runner::{RealRunner, Runner},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "
ALTER TABLE artifact_staging RENAME TO artifact_staging_v10;
CREATE TABLE artifact_staging (
 attempt TEXT PRIMARY KEY REFERENCES jobs(id), epoch INTEGER NOT NULL,
 manifest_hash TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('creating','ready','retired'))
) STRICT;
INSERT INTO artifact_staging SELECT * FROM artifact_staging_v10;
DROP TABLE artifact_staging_v10;
CREATE TABLE artifact_copy_retirements (
 attempt TEXT PRIMARY KEY REFERENCES jobs(id), operation TEXT NOT NULL UNIQUE,
 request TEXT NOT NULL, scope_hash TEXT NOT NULL, directory TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','removing','removed')),
 receipt TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT; PRAGMA user_version=11;";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub attempt: String,
    pub expected_epoch: u64,
    pub expected_manifest_hash: String,
    pub processes_confirmed_stopped: bool,
    pub retain_receipts_remove_inputs: bool,
    pub reason: String,
}

#[derive(Serialize)]
struct Scope {
    authority: cleanup::Scope,
    candidate: Candidate,
    path: PathBuf,
    staging: (u64, String, String, String),
    checkout: CheckoutPlan,
}

fn scope(c: &Connection, config: &ExecutorConfig, r: &Request) -> Result<Scope> {
    let authority = cleanup::scope_in(c, &r.attempt, r.expected_epoch)?;
    ensure!(
        authority.job.0 == r.expected_epoch
            && authority.job.1 == "stopped"
            && cleanup::applied_stop(&authority, &r.attempt, r.expected_epoch)?,
        "exact stopped job and controller-applied stop receipt required"
    );
    let order: ExecuteOrder = serde_json::from_str(&authority.job.2)?;
    ensure!(
        order.attempt.id == r.attempt
            && order.attempt.role == "reviewer"
            && order.attempt.node == authority.authority.1
            && order.attempt.epoch <= r.expected_epoch,
        "review assignment scope changed"
    );
    let (checkout, never_launched) = match serde_json::from_str::<CheckoutPlan>(&authority.job.3) {
        Ok(plan) => (plan, true),
        Err(_) => {
            let launch: LaunchPlan = serde_json::from_str(&authority.job.3)?;
            let never = !launch.launch_started && !launch.wrapper_started;
            (launch.checkout, never)
        }
    };
    let exited = match &authority.process {
        Some((epoch, state, doc)) => {
            let v: Value = serde_json::from_str(doc)?;
            *epoch > 0
                && *epoch <= r.expected_epoch
                && state == "exited"
                && v["attempt"] == r.attempt
                && v["epoch"] == *epoch
                && v["scope"] == "native_process"
        }
        None => never_launched,
    };
    ensure!(exited, "reviewer native process exit remains unresolved");
    ensure!(
        config.herdr_socket == checkout.socket
            && config.herdr_binary == checkout.herdr_binary
            && std::fs::canonicalize(
                config
                    .repositories
                    .get(&order.repo)
                    .context("repository not configured")?
            )? == checkout.repo
            && std::fs::canonicalize(&config.worktree_root)?.join(&r.attempt) == checkout.worktree
            && checkout.branch == format!("hs/{}/{}", order.attempt.run, r.attempt)
            && checkout.base_oid == order.attempt.base_oid
            && checkout.creation_started
            && checkout.binding.is_some(),
        "review checkout ownership or configuration changed"
    );
    let candidate = order
        .review
        .as_ref()
        .and_then(|review| review.artifact.clone())
        .context("artifact reviewer required")?;
    ensure!(
        candidate.manifest_hash == r.expected_manifest_hash,
        "artifact manifest changed"
    );
    super::artifact_review::verify_cached(c, &order, &candidate)?;
    let staging: (u64, String, String, String) = c.query_row(
        "SELECT epoch,manifest_hash,path,state FROM artifact_staging WHERE attempt=?1",
        [&r.attempt],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let path = checkout
        .worktree
        .join(format!(".herdr-supervisor-artifacts-{}", r.attempt));
    ensure!(
        staging.0 == order.attempt.epoch
            && staging.1 == candidate.manifest_hash
            && Path::new(&staging.2) == path
            && staging.3 == "ready",
        "artifact staging ownership or state changed"
    );
    Ok(Scope {
        authority,
        candidate,
        path,
        staging,
        checkout,
    })
}
fn hash(s: &Scope) -> Result<String> {
    Ok(digest(&serde_json::to_vec(s)?))
}

pub fn retire(state: &Path, config: &ExecutorConfig, r: &Request) -> Result<Value> {
    retire_with(state, config, r, &RealRunner)
}
pub(super) fn retire_with(
    state: &Path,
    config: &ExecutorConfig,
    r: &Request,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(&r.operation)?;
    uuid::Uuid::parse_str(&r.attempt)?;
    artifacts::hash(&r.expected_manifest_hash)?;
    ensure!(
        r.expected_epoch > 0 && r.processes_confirmed_stopped && r.retain_receipts_remove_inputs,
        "explicit process/receipt-retention confirmations and epoch required"
    );
    ensure!(
        !r.reason.trim().is_empty()
            && r.reason.len() <= 4096
            && !r.reason.chars().any(char::is_control),
        "bounded single-line operator reason required"
    );
    let mut c = super::reception::local_db(state, true, true)?;
    let _lease = super::api::lock(state, "execution.lock")?;
    let frozen = serde_json::to_string(r)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure!(
        cleanup::scope_in(&tx, &r.attempt, r.expected_epoch)?.job.0 == r.expected_epoch,
        "attempt epoch changed"
    );
    let prior:Option<(String,String,String,String,Option<String>)>=tx.query_row("SELECT request,scope_hash,directory,state,receipt FROM artifact_copy_retirements WHERE attempt=?1 OR operation=?2",params![r.attempt,r.operation],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
    if let Some((saved, _, _, phase, receipt)) = &prior {
        ensure!(
            saved == &frozen,
            "artifact copy retirement decision changed"
        );
        if phase == "removed" {
            return Ok(serde_json::from_str(
                receipt.as_deref().context("missing retirement receipt")?,
            )?);
        }
    }
    let s = scope(&tx, config, r)?;
    let scope_hash = hash(&s)?;
    if let Some((_, old, _, _, _)) = &prior {
        ensure!(old == &scope_hash, "reviewer retirement authority changed");
    }
    tx.commit()?;
    cleanup::git_identity(runner, &s.checkout)?;
    executor::observe_pane(
        &Herdr::new(&s.checkout.herdr_binary, &s.checkout.socket, runner),
        &s.checkout,
    )?;
    let directory: Identity = if let Some((_, _, id, _, _)) = &prior {
        serde_json::from_str(id)?
    } else {
        artifact_removal::inspect(&s.path, &s.candidate)?
    };
    let marker = serde_json::to_vec(
        &json!({"operation":r.operation,"manifest":r.expected_manifest_hash,"directory":directory}),
    )?;
    if prior.is_none() {
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            hash(&scope(&tx, config, r)?)? == scope_hash,
            "reviewer authority changed during inspection"
        );
        tx.execute(
            "INSERT INTO artifact_copy_retirements VALUES (?1,?2,?3,?4,?5,'prepared',NULL,?6,?6)",
            params![
                r.attempt,
                r.operation,
                frozen,
                scope_hash,
                serde_json::to_string(&directory)?,
                now()
            ],
        )?;
        tx.commit()?;
    }
    if !prior.as_ref().is_some_and(|p| p.3 == "removing") {
        artifact_removal::prepare(&s.path, &s.candidate, &directory, &marker)?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure!(
            hash(&scope(&tx, config, r)?)? == scope_hash,
            "reviewer authority changed before removal"
        );
        ensure!(tx.execute("UPDATE artifact_copy_retirements SET state='removing',updated=?2 WHERE attempt=?1 AND state='prepared'",params![r.attempt,now()])?==1,"retirement phase changed");
        tx.commit()?;
    }
    artifact_removal::remove(&s.path, &s.candidate, &directory, &marker).context(
        "reviewer input retirement remains removing; earlier removals are not rolled back",
    )?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure!(
        hash(&scope(&tx, config, r)?)? == scope_hash,
        "reviewer authority changed after removal"
    );
    let receipt = json!({"attempt":r.attempt,"operation":r.operation,"epoch":r.expected_epoch,"manifest_hash":r.expected_manifest_hash,"state":"removed","inputs_removed":true,"receipts_retained":true,"worktree_removed":false,"observed_at":now(),"process_evidence":"local_operator_attestation_and_recorded_exit_or_no_launch"});
    ensure!(
        tx.execute(
            "UPDATE artifact_staging SET state='retired' WHERE attempt=?1 AND state='ready'",
            [&r.attempt]
        )? == 1,
        "staging state changed"
    );
    ensure!(tx.execute("UPDATE artifact_copy_retirements SET state='removed',receipt=?2,updated=?3 WHERE attempt=?1 AND state='removing'",params![r.attempt,receipt.to_string(),now()])?==1,"retirement phase changed");
    tx.commit()?;
    Ok(receipt)
}
