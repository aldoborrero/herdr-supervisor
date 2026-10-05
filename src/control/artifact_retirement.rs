//! Retire terminal verifier copies while retaining authoritative artifact bytes.
use super::{
    artifact_removal::{self, Identity},
    artifacts,
    model::{Attempt, Integration, Run, digest},
    store::{self, Store, document, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "
CREATE TABLE artifact_retirements (
 integration TEXT PRIMARY KEY REFERENCES integrations(id), operation TEXT NOT NULL UNIQUE,
 request TEXT NOT NULL, scope_hash TEXT NOT NULL, directory TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','removing','removed')),
 receipt TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT; PRAGMA user_version=14;";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub integration: String,
    pub expected_revision: u64,
    pub expected_manifest_hash: String,
    pub processes_confirmed_stopped: bool,
    pub retain_source_remove_inputs: bool,
    pub reason: String,
}

#[derive(Serialize)]
struct Scope {
    integration: Integration,
    run: Run,
    path: PathBuf,
    checks: Vec<(String, String, String)>,
}

fn authority(c: &Connection, token: &str) -> Result<String> {
    let p = store::authenticate(c, token)?;
    store::own(&p)?;
    Ok(p.id)
}
fn scope(c: &Connection, root: &Path, r: &Request) -> Result<Scope> {
    let i = super::verification::integration(c, &r.integration)?;
    let candidate = i.artifact.as_ref().context("artifact candidate required")?;
    candidate.validate()?;
    ensure!(
        i.revision == r.expected_revision && candidate.manifest_hash == r.expected_manifest_hash,
        "artifact revision or manifest changed"
    );
    let run: Run = document(c, "runs", &i.run)?;
    ensure!(
        ["completed", "cancelled"].contains(&run.state.as_str()),
        "terminal run required before artifact retirement"
    );
    ensure!(
        [
            "applied",
            "superseded",
            "checks_failed",
            "changes_requested"
        ]
        .contains(&i.state.as_str()),
        "known terminal artifact integration required"
    );
    ensure!(
        super::recovery::blockers(c, &run.id)?.is_empty(),
        "run has unresolved effects"
    );
    let active: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM repository_bindings WHERE run=?1 AND active=1)",
        [&run.id],
        |row| row.get(0),
    )?;
    ensure!(!active, "repository binding has not been released");
    let (path, state): (String, String) = c.query_row(
        "SELECT path,state FROM verification_worktrees WHERE integration=?1",
        [&i.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let path = PathBuf::from(path);
    ensure!(
        state == "ready" && path == root.join("verification").join(&i.id).join("inputs"),
        "artifact input ownership or state changed"
    );
    let checks = c
        .prepare("SELECT id,state,doc FROM check_runs WHERE integration=?1 ORDER BY id")?
        .query_map([&i.id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?;
    ensure!(
        checks
            .iter()
            .all(|(_, state, _)| ["passed", "failed"].contains(&state.as_str())),
        "artifact check outcome unresolved"
    );
    let source: Attempt = document(c, "attempts", &i.attempt)?;
    ensure!(
        artifacts::candidate(c, &source)? == *candidate,
        "authoritative artifact source changed or missing"
    );
    Ok(Scope {
        integration: i,
        run,
        path,
        checks,
    })
}
fn hash(s: &Scope) -> Result<String> {
    Ok(digest(&serde_json::to_vec(s)?))
}
fn journal(c: &Connection, p: &str, r: &Request, kind: &str, doc: &Value) -> Result<()> {
    c.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES (?1,?2,?3,?4,?5,?6)",params![p,r.operation,r.integration,kind,doc.to_string(),now()])?;
    Ok(())
}

pub fn retire(store: &mut Store, token: &str, r: &Request) -> Result<Value> {
    uuid::Uuid::parse_str(&r.operation)?;
    uuid::Uuid::parse_str(&r.integration)?;
    artifacts::hash(&r.expected_manifest_hash)?;
    ensure!(
        r.expected_revision > 0 && r.processes_confirmed_stopped && r.retain_source_remove_inputs,
        "explicit process/source-retention confirmations and revision required"
    );
    ensure!(
        !r.reason.trim().is_empty()
            && r.reason.len() <= 4096
            && !r.reason.chars().any(char::is_control),
        "bounded single-line operator reason required"
    );
    authority(&store.connection, token)?;
    let _lease = super::api::lock(&store.dir, "git-executor.lock")?;
    let frozen = serde_json::to_string(r)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    authority(&tx, token)?;
    let prior:Option<(String,String,String,String,Option<String>)>=tx.query_row("SELECT request,scope_hash,directory,state,receipt FROM artifact_retirements WHERE integration=?1 OR operation=?2",params![r.integration,r.operation],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
    if let Some((saved, _, _, state, receipt)) = &prior {
        ensure!(saved == &frozen, "artifact retirement decision changed");
        if state == "removed" {
            return Ok(serde_json::from_str(
                receipt.as_deref().context("missing retirement receipt")?,
            )?);
        }
    }
    let s = scope(&tx, &store.dir, r)?;
    let scope_hash = hash(&s)?;
    if let Some((_, old, _, _, _)) = &prior {
        ensure!(old == &scope_hash, "artifact retirement authority changed");
    }
    tx.commit()?;
    let candidate = s
        .integration
        .artifact
        .as_ref()
        .context("artifact candidate missing")?;
    let directory: Identity = if let Some((_, _, identity, _, _)) = &prior {
        serde_json::from_str(identity)?
    } else {
        artifact_removal::inspect(&s.path, candidate)?
    };
    let marker = serde_json::to_vec(
        &json!({"operation":r.operation,"manifest":r.expected_manifest_hash,"directory":directory}),
    )?;
    if prior.is_none() {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, &store.dir, r)?)? == scope_hash,
            "artifact authority changed during inspection"
        );
        tx.execute(
            "INSERT INTO artifact_retirements VALUES (?1,?2,?3,?4,?5,'prepared',NULL,?6,?6)",
            params![
                r.integration,
                r.operation,
                frozen,
                scope_hash,
                serde_json::to_string(&directory)?,
                now()
            ],
        )?;
        journal(
            &tx,
            &p,
            r,
            "artifact_retirement_prepared",
            &serde_json::to_value(r)?,
        )?;
        tx.commit()?;
    }
    if !prior.as_ref().is_some_and(|p| p.3 == "removing") {
        artifact_removal::prepare(&s.path, candidate, &directory, &marker)?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let p = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, &store.dir, r)?)? == scope_hash,
            "artifact authority changed before removal"
        );
        ensure!(tx.execute("UPDATE artifact_retirements SET state='removing',updated=?2 WHERE integration=?1 AND state='prepared'",params![r.integration,now()])?==1,"retirement phase changed");
        journal(
            &tx,
            &p,
            r,
            "artifact_retirement_issued",
            &json!({"manifest":r.expected_manifest_hash}),
        )?;
        tx.commit()?;
    }
    // A partial deletion is resumed only inside the same marked directory, with
    // remaining file hashes rechecked and the original bytes still in SQLite.
    artifact_removal::remove(&s.path, candidate, &directory, &marker)
        .context("artifact retirement remains removing; already removed copies are not restored")?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let p = authority(&tx, token)?;
    ensure!(
        hash(&scope(&tx, &store.dir, r)?)? == scope_hash,
        "artifact authority changed after removal"
    );
    let receipt = json!({"integration":r.integration,"operation":r.operation,"state":"removed","inputs_removed":true,
        "source_retained":true,"manifest_hash":r.expected_manifest_hash,"observed_at":now(),"process_evidence":"local_operator_attestation_and_terminal_run"});
    ensure!(tx.execute("UPDATE verification_worktrees SET state='retired' WHERE integration=?1 AND state='ready'",[&r.integration])?==1,"artifact input state changed");
    ensure!(tx.execute("UPDATE artifact_retirements SET state='removed',receipt=?2,updated=?3 WHERE integration=?1 AND state='removing'",params![r.integration,receipt.to_string(),now()])?==1,"retirement phase changed");
    journal(&tx, &p, r, "artifact_retirement_completed", &receipt)?;
    tx.commit()?;
    Ok(receipt)
}
