//! Owner-authorized retirement of terminal code-verification worktrees.
//! Keep receipts and commits; an operator attests exclusive process quiescence.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    executor::git,
    git_pipeline,
    model::{Integration, Run, digest, oid},
    store::{self, Store, document, now},
};
use crate::runner::{RealRunner, Runner};

pub const SCHEMA: &str = "
CREATE TABLE verification_retirements (
 integration TEXT PRIMARY KEY REFERENCES integrations(id), operation TEXT NOT NULL UNIQUE,
 request TEXT NOT NULL, scope_hash TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','removing','removed')),
 receipt TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT;
PRAGMA user_version=12;";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub integration: String,
    pub expected_revision: u64,
    pub expected_candidate: String,
    pub processes_confirmed_stopped: bool,
    pub retain_commits_remove_checkout: bool,
    pub reason: String,
}

#[derive(Serialize)]
struct Scope {
    integration: Integration,
    run: Run,
    path: PathBuf,
    repo: PathBuf,
    common: PathBuf,
    checks: Vec<(String, String, String)>,
}

fn authority(c: &Connection, token: &str) -> Result<String> {
    let principal = store::authenticate(c, token)?;
    store::own(&principal)?;
    Ok(principal.id)
}

fn scope(c: &Connection, root: &Path, request: &Request) -> Result<Scope> {
    let i = super::verification::integration(c, &request.integration)?;
    ensure!(
        i.revision == request.expected_revision
            && i.candidate_oid.as_deref() == Some(&request.expected_candidate)
            && i.artifact.is_none(),
        "verification candidate or revision changed; code candidate required"
    );
    let run: Run = document(c, "runs", &i.run)?;
    ensure!(
        ["completed", "cancelled"].contains(&run.state.as_str()),
        "run must be terminal before verification retirement"
    );
    ensure!(
        [
            "applied",
            "superseded",
            "checks_failed",
            "conflict",
            "changes_requested"
        ]
        .contains(&i.state.as_str()),
        "integration is not in a known terminal state"
    );
    ensure!(
        super::recovery::blockers(c, &run.id)?.is_empty(),
        "run has unresolved effects"
    );
    let (repo, common, state, active): (String, String, String, bool) = c.query_row(
        "SELECT path,common_dir,state,active FROM repository_bindings WHERE run=?1",
        [&run.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        state == "ready" && !active,
        "repository binding has not been released"
    );
    let (path, state): (String, String) = c.query_row(
        "SELECT path,state FROM verification_worktrees WHERE integration=?1",
        [&i.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let path = PathBuf::from(path);
    ensure!(
        path == root.join("verification").join(&i.id).join("checkout") && state == "ready",
        "verification checkout ownership or state changed"
    );
    let checks: Vec<(String, String, String)> = c
        .prepare("SELECT id,state,doc FROM check_runs WHERE integration=?1 ORDER BY id")?
        .query_map([&i.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        checks
            .iter()
            .all(|(_, state, _)| ["passed", "failed"].contains(&state.as_str())),
        "verification check outcome is unresolved"
    );
    Ok(Scope {
        integration: i,
        run,
        path,
        repo: repo.into(),
        common: common.into(),
        checks,
    })
}

fn hash(scope: &Scope) -> Result<String> {
    Ok(digest(&serde_json::to_vec(scope)?))
}

fn repository(runner: &dyn Runner, scope: &Scope, config: &git_pipeline::Config) -> Result<()> {
    let configured = &config
        .repositories
        .get(&scope.run.repo)
        .context("repository authorization missing")?
        .path;
    ensure!(
        std::fs::canonicalize(configured)? == scope.repo
            && std::fs::canonicalize(&scope.repo)? == scope.repo,
        "repository configuration or path changed"
    );
    let common = git(
        runner,
        &scope.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    ensure!(
        std::fs::canonicalize(common)? == scope.common,
        "repository identity changed"
    );
    Ok(())
}

fn clean(runner: &dyn Runner, s: &Scope, request: &Request) -> Result<()> {
    ensure!(
        std::fs::canonicalize(&s.path)? == s.path
            && std::fs::symlink_metadata(s.path.join(".git"))?.is_file(),
        "verification checkout path was substituted"
    );
    ensure!(
        std::fs::canonicalize(git(
            runner,
            &s.path,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"]
        )?)? == s.common,
        "verification checkout repository changed"
    );
    ensure!(
        git(runner, &s.path, &["rev-parse", "HEAD"])? == request.expected_candidate,
        "verification HEAD changed"
    );
    git_pipeline::detached_ref(runner, &s.path, "HEAD")?;
    ensure!(
        git(
            runner,
            &s.path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
                "--ignore-submodules=none"
            ]
        )?
        .is_empty(),
        "verification checkout has retained files or changes"
    );
    ensure!(
        git(runner, &s.path, &["ls-files", "-v", "-z"])?
            .split('\0')
            .filter(|s| !s.is_empty())
            .all(|s| s.starts_with("H ")),
        "verification index has hidden entries"
    );
    ensure!(
        !git(runner, &s.path, &["ls-files", "--stage", "-z"])?
            .split('\0')
            .any(|s| s.starts_with("160000 ")),
        "verification checkout has submodules"
    );
    Ok(())
}

fn reference(request: &Request) -> String {
    format!(
        "refs/herdr-supervisor/retained-verification/{}",
        request.integration
    )
}

fn absent(runner: &dyn Runner, s: &Scope, request: &Request) -> Result<bool> {
    git_pipeline::detached_ref(runner, &s.repo, &reference(request))?;
    ensure!(
        git(
            runner,
            &s.repo,
            &["rev-parse", "--verify", &reference(request)]
        )? == request.expected_candidate,
        "verification retention reference changed"
    );
    match std::fs::symlink_metadata(&s.path) {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let listed = git(runner, &s.repo, &["worktree", "list", "--porcelain", "-z"])?;
    let target = format!(
        "worktree {}",
        s.path.to_str().context("non UTF-8 checkout")?
    );
    Ok(!listed.split('\0').any(|entry| entry == target))
}

fn journal(
    c: &Connection,
    principal: &str,
    request: &Request,
    kind: &str,
    doc: &Value,
) -> Result<()> {
    c.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES (?1,?2,?3,?4,?5,?6)",
        params![principal,request.operation,request.integration,kind,doc.to_string(),now()])?;
    Ok(())
}

pub fn retire(
    store: &mut Store,
    config: &git_pipeline::Config,
    token: &str,
    request: &Request,
) -> Result<Value> {
    retire_with(store, config, token, request, &RealRunner)
}

pub(super) fn retire_with(
    store: &mut Store,
    config: &git_pipeline::Config,
    token: &str,
    request: &Request,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(&request.operation)?;
    uuid::Uuid::parse_str(&request.integration)?;
    oid(&request.expected_candidate)?;
    ensure!(
        request.expected_revision > 0
            && request.processes_confirmed_stopped
            && request.retain_commits_remove_checkout,
        "explicit process/retention confirmations and current revision required"
    );
    ensure!(
        !request.reason.trim().is_empty()
            && request.reason.len() <= 4096
            && !request.reason.chars().any(char::is_control),
        "a bounded single-line operator reason is required"
    );
    authority(&store.connection, token)?;
    let _lease = super::api::lock(&store.dir, "git-executor.lock")?;
    let frozen = serde_json::to_string(request)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    authority(&tx, token)?;
    let prior: Option<(String,String,String,Option<String>)> = tx.query_row(
        "SELECT request,scope_hash,state,receipt FROM verification_retirements WHERE integration=?1 OR operation=?2",
        params![request.integration,request.operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((saved, _, status, receipt)) = &prior {
        ensure!(saved == &frozen, "verification retirement decision changed");
        if status == "removed" {
            return Ok(serde_json::from_str(
                receipt.as_deref().context("missing retirement receipt")?,
            )?);
        }
    }
    let s = scope(&tx, &store.dir, request)?;
    let scope_hash = hash(&s)?;
    if let Some((_, saved, _, _)) = &prior {
        ensure!(
            saved == &scope_hash,
            "verification retirement authority changed"
        );
    }
    tx.commit()?;
    repository(runner, &s, config)?;
    if !prior.as_ref().is_some_and(|p| p.2 == "removing") {
        clean(runner, &s, request)?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, &store.dir, request)?)? == scope_hash,
            "verification authority changed during inspection"
        );
        if prior.is_none() {
            tx.execute(
                "INSERT INTO verification_retirements VALUES (?1,?2,?3,?4,'prepared',NULL,?5,?5)",
                params![
                    request.integration,
                    request.operation,
                    frozen,
                    scope_hash,
                    now()
                ],
            )?;
            journal(
                &tx,
                &principal,
                request,
                "verification_retirement_prepared",
                &serde_json::to_value(request)?,
            )?;
        }
        tx.commit()?;
        let reference = reference(request);
        git_pipeline::detached_ref(runner, &s.repo, &reference)?;
        git_pipeline::pin(runner, &s.repo, &reference, &request.expected_candidate)?;
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, &store.dir, request)?)? == scope_hash,
            "verification authority changed before removal"
        );
        ensure!(tx.execute("UPDATE verification_retirements SET state='removing',updated=?2 WHERE integration=?1 AND state='prepared'",
            params![request.integration,now()])? == 1,"retirement phase changed");
        journal(
            &tx,
            &principal,
            request,
            "verification_retirement_issued",
            &json!({"candidate":request.expected_candidate}),
        )?;
        tx.commit()?;
        let _outcome = git(
            runner,
            &s.repo,
            &[
                "worktree",
                "remove",
                s.path.to_str().context("non UTF-8 checkout")?,
            ],
        );
    }
    repository(runner, &s, config)?;
    if absent(runner, &s, request).unwrap_or(false) {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authority(&tx, token)?;
        ensure!(
            hash(&scope(&tx, &store.dir, request)?)? == scope_hash,
            "verification authority changed after removal"
        );
        let receipt = json!({"integration":request.integration,"operation":request.operation,"state":"removed","resources_deleted":true,
            "candidate":request.expected_candidate,"retained_ref":reference(request),"observed_at":now(),"process_evidence":"local_operator_attestation_and_terminal_run"});
        ensure!(tx.execute("UPDATE verification_worktrees SET state='retired' WHERE integration=?1 AND state='ready'",[&request.integration])? == 1,"checkout state changed");
        ensure!(tx.execute("UPDATE verification_retirements SET state='removed',receipt=?2,updated=?3 WHERE integration=?1 AND state='removing'",
            params![request.integration,receipt.to_string(),now()])? == 1,"retirement phase changed");
        journal(
            &tx,
            &principal,
            request,
            "verification_retirement_completed",
            &receipt,
        )?;
        tx.commit()?;
        return Ok(receipt);
    }
    Ok(
        json!({"integration":request.integration,"operation":request.operation,"state":"removing","resources_deleted":null,
        "outcome":"unresolved","automatic_retry":false}),
    )
}
