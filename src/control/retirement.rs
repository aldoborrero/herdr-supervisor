//! Explicit local-operator retirement. The operator supplies process quiescence;
//! native exit/idle-shell observations alone never authorize deleting a checkout.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    cleanup::{Scope, scope},
    executor::{CheckoutPlan, ExecutorConfig, git},
    model::{digest, oid},
    runtime::LaunchPlan,
    store::now,
};
use crate::{
    herdr::Herdr,
    runner::{RealRunner, Runner},
};

pub const SCHEMA: &str = "
CREATE TABLE checkout_retirements (
 attempt TEXT PRIMARY KEY REFERENCES jobs(id), operation TEXT NOT NULL UNIQUE,
 request TEXT NOT NULL, plan TEXT NOT NULL, scope_hash TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('prepared','removing','removed')),
 receipt TEXT, created INTEGER NOT NULL, updated INTEGER NOT NULL
) STRICT;
PRAGMA user_version=10;";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub operation: String,
    pub attempt: String,
    pub expected_epoch: u64,
    pub expected_head: String,
    /// A local operator attestation, never inferred by a worker or coordinator.
    pub processes_confirmed_stopped: bool,
    /// Explicit retention decision: preserve all commits but remove this checkout.
    pub retain_commits_remove_checkout: bool,
    pub reason: String,
}

fn plan(s: &Scope) -> Result<CheckoutPlan> {
    match serde_json::from_str::<CheckoutPlan>(&s.job.3) {
        Ok(p) => Ok(p),
        Err(_) => Ok(serde_json::from_str::<LaunchPlan>(&s.job.3)?.checkout),
    }
}

fn scope_hash(s: &Scope) -> Result<String> {
    Ok(digest(&serde_json::to_vec(s)?))
}

fn retained_ref(request: &Request) -> String {
    format!("refs/herdr-supervisor/retained/{}", request.attempt)
}

fn repository(runner: &dyn Runner, plan: &CheckoutPlan) -> Result<()> {
    ensure!(
        std::fs::canonicalize(&plan.repo)? == plan.repo,
        "repository path changed"
    );
    let common = git(
        runner,
        &plan.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    ensure!(
        std::fs::canonicalize(common)? == plan.common_dir,
        "repository identity changed"
    );
    Ok(())
}

fn current_head(runner: &dyn Runner, plan: &CheckoutPlan, request: &Request) -> Result<()> {
    repository(runner, plan)?;
    ensure!(
        git(
            runner,
            &plan.repo,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", plan.branch)
            ]
        )? == request.expected_head,
        "retained branch changed"
    );
    Ok(())
}

fn absent(runner: &dyn Runner, plan: &CheckoutPlan, request: &Request) -> Result<bool> {
    current_head(runner, plan, request)?;
    ensure!(
        git(
            runner,
            &plan.repo,
            &["rev-parse", "--verify", &retained_ref(request)]
        )? == request.expected_head,
        "retained commit reference changed"
    );
    match std::fs::symlink_metadata(&plan.worktree) {
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let listed = git(
        runner,
        &plan.repo,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    let entry = format!(
        "worktree {}",
        plan.worktree.to_str().context("non UTF-8 checkout")?
    );
    if listed.split('\0').any(|line| line == entry) {
        return Ok(false);
    }
    let saved = plan.binding.as_ref().context("missing binding")?;
    let herdr = Herdr::new(&plan.herdr_binary, &plan.socket, runner);
    if herdr.workspace_list()?.iter().any(|w| {
        w.workspace_id == saved.workspace
            || w.worktree
                .as_ref()
                .is_some_and(|t| Path::new(&t.checkout_path) == plan.worktree)
    }) {
        return Ok(false);
    }
    Ok(!herdr.pane_list()?.iter().any(|p| {
        p.workspace_id == saved.workspace
            || p.pane_id == saved.pane
            || p.terminal_id == saved.terminal
            || Path::new(&p.cwd) == plan.worktree
    }))
}

pub fn retire(state: &Path, config: &ExecutorConfig, request: &Request) -> Result<Value> {
    retire_with(state, config, request, &RealRunner)
}

pub(super) fn retire_with(
    state: &Path,
    config: &ExecutorConfig,
    request: &Request,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(&request.attempt)?;
    uuid::Uuid::parse_str(&request.operation)?;
    oid(&request.expected_head)?;
    ensure!(
        request.expected_epoch > 0
            && request.processes_confirmed_stopped
            && request.retain_commits_remove_checkout,
        "explicit local operator process and retention confirmations required"
    );
    ensure!(
        !request.reason.trim().is_empty()
            && request.reason.len() <= 4096
            && !request.reason.chars().any(char::is_control),
        "a bounded single-line operator reason is required"
    );
    // Validate existing private state before taking a lease; missing state is not initialized.
    let mut c = super::reception::local_db(state, true, true)?;
    let _lease = super::api::lock(state, "execution.lock")?;
    let initial = scope(&mut c, &request.attempt, request.expected_epoch)?;
    ensure!(
        initial.job.0 == request.expected_epoch,
        "attempt epoch changed"
    );
    let frozen = serde_json::to_string(request)?;
    let prior: Option<(String,String,String,String,Option<String>)> = c.query_row(
        "SELECT request,plan,scope_hash,state,receipt FROM checkout_retirements WHERE attempt=?1 OR operation=?2",
        params![request.attempt,request.operation], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if let Some((saved, _, _, status, receipt)) = &prior {
        ensure!(
            saved == &frozen,
            "retirement operation or attempt already has another decision"
        );
        if status == "removed" {
            return Ok(serde_json::from_str(
                receipt.as_deref().context("missing retirement receipt")?,
            )?);
        }
    }
    let checkout = plan(&initial)?;
    let order: super::node::ExecuteOrder = serde_json::from_str(&initial.job.2)?;
    let scope_digest = scope_hash(&initial)?;
    if let Some((_, saved, hash, _, _)) = &prior {
        ensure!(
            serde_json::from_str::<CheckoutPlan>(saved)? == checkout && hash == &scope_digest,
            "retirement authority or resource plan changed"
        );
    }
    // Even reconciliation must use the original explicitly configured endpoint/repository.
    ensure!(
        config.herdr_socket == checkout.socket
            && config.herdr_binary == checkout.herdr_binary
            && config
                .repositories
                .get(&order.repo)
                .and_then(|repo| std::fs::canonicalize(repo).ok())
                .as_ref()
                == Some(&checkout.repo)
            && std::fs::canonicalize(&config.worktree_root)?.join(&request.attempt)
                == checkout.worktree,
        "executor configuration changed"
    );
    let removing = prior.as_ref().is_some_and(|p| p.3 == "removing");
    if !removing {
        let report = super::cleanup::inspect_with(
            state,
            config,
            &request.attempt,
            request.expected_epoch,
            runner,
        )?;
        for check in report["checks"].as_array().context("missing inspection")? {
            let name = check["check"].as_str().context("invalid inspection")?;
            if ![
                "no_native_launch_intent",
                "descendants_and_background_processes_exited",
                "retention_policy_satisfied",
            ]
            .contains(&name)
            {
                ensure!(check["status"] == "passed", "checkout retained: {name}");
            }
        }
        ensure!(
            report["head_oid"] == request.expected_head,
            "checkout HEAD changed"
        );
        let process_stopped = match &initial.process {
            Some((epoch, status, doc)) => {
                let receipt: Value = serde_json::from_str(doc)?;
                *epoch > 0
                    && *epoch <= request.expected_epoch
                    && status == "exited"
                    && receipt["attempt"] == request.attempt
                    && receipt["epoch"] == *epoch
                    && receipt["scope"] == "native_process"
            }
            None => report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["check"] == "no_native_launch_intent" && c["status"] == "passed"),
        };
        ensure!(process_stopped, "recorded native exit remains unresolved");
        ensure!(
            scope(&mut c, &request.attempt, request.expected_epoch)? == initial,
            "authority changed during inspection"
        );
        if prior.is_none() {
            c.execute(
                "INSERT INTO checkout_retirements VALUES (?1,?2,?3,?4,?5,'prepared',NULL,?6,?6)",
                params![
                    request.attempt,
                    request.operation,
                    frozen,
                    serde_json::to_string(&checkout)?,
                    scope_digest,
                    now()
                ],
            )?;
        }
        current_head(runner, &checkout, request)?;
        let reference = retained_ref(request);
        // Exact CAS creation; never repoint an existing retention reference.
        if git(
            runner,
            &checkout.repo,
            &["show-ref", "--verify", "--hash", &reference],
        )
        .ok()
        .as_deref()
            != Some(&request.expected_head)
        {
            git(
                runner,
                &checkout.repo,
                &[
                    "update-ref",
                    &reference,
                    &request.expected_head,
                    &"0".repeat(request.expected_head.len()),
                ],
            )?;
        }
        // Persist uncertainty before invoking Herdr. A lost reply never causes a second removal.
        ensure!(c.execute("UPDATE checkout_retirements SET state='removing',updated=?2 WHERE attempt=?1 AND state='prepared'",
            params![request.attempt,now()])? == 1, "retirement phase changed");
        let herdr = Herdr::new(&checkout.herdr_binary, &checkout.socket, runner);
        // The native error may include terminal output. Keep it out of the durable/operator report.
        let _ = herdr.worktree_remove(
            &checkout
                .binding
                .as_ref()
                .context("missing binding")?
                .workspace,
        );
    }
    if absent(runner, &checkout, request).unwrap_or(false)
        && scope(&mut c, &request.attempt, request.expected_epoch)? == initial
    {
        let receipt = json!({"attempt":request.attempt,"operation":request.operation,"epoch":request.expected_epoch,
            "state":"removed","resources_deleted":true,"branch":checkout.branch,"retained_ref":retained_ref(request),
            "head_oid":request.expected_head,"process_evidence":"local_operator_attestation_and_recorded_exit_or_no_launch",
            "observed_at":now()});
        ensure!(c.execute("UPDATE checkout_retirements SET state='removed',receipt=?2,updated=?3 WHERE attempt=?1 AND state='removing'",
            params![request.attempt,receipt.to_string(),now()])? == 1, "retirement phase changed");
        return Ok(receipt);
    }
    Ok(
        json!({"attempt":request.attempt,"operation":request.operation,"state":"removing",
        "resources_deleted":null,"outcome":"unresolved","automatic_retry":false,
        "action":"inspect the saved resources; rerunning this same request only reconciles the outcome"}),
    )
}
