//! Read-only preflight for eventual owned-resource retirement. A clean checkout
//! and an idle foreground shell never establish that all descendants have exited.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Value, json};

use super::{
    executor::{CheckoutPlan, ExecutorConfig, git},
    identity::SignedRecord,
    model::{Attempt, Mutation, Request, oid},
    node::ExecuteOrder,
    runtime::LaunchPlan,
};
use crate::{
    herdr::Herdr,
    runner::{RealRunner, Runner},
};

#[derive(PartialEq)]
struct Scope {
    authority: (String, String, String, u64),
    job: (u64, String, String, String),
    process: Option<(u64, String, String)>,
    stop: Option<(String, String, Option<String>)>,
}

fn scope(c: &mut Connection, id: &str, epoch: u64) -> Result<Scope> {
    let tx = c.transaction_with_behavior(TransactionBehavior::Deferred)?;
    ensure!(
        tx.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))? == 9,
        "unsupported cleanup inspection schema; run the current node service migration first"
    );
    let result = Scope {
        authority: tx.query_row("SELECT owner,node,key,epoch FROM authority WHERE singleton=1", [],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?,
        job: tx.query_row("SELECT epoch,state,doc,checkpoint FROM jobs WHERE id=?1", [id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?,
        process: tx.query_row("SELECT epoch,state,doc FROM runtime_processes WHERE attempt=?1", [id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?,
        stop: tx.query_row("SELECT state,wire,response FROM events WHERE json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.operation_id')=?1 AND state='controller_applied' ORDER BY rowid DESC LIMIT 1",
            [format!("stopped-{id}-{epoch}")], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?,
    };
    tx.commit()?;
    Ok(result)
}

fn applied_stop(s: &Scope, id: &str, epoch: u64) -> Result<bool> {
    let Some((state, wire, Some(response))) = &s.stop else {
        return Ok(false);
    };
    if state != "controller_applied" {
        return Ok(false);
    }
    let signed: SignedRecord = serde_json::from_str(wire)?;
    let request: Request = serde_json::from_value(signed.record.payload)?;
    let stopped: Attempt = serde_json::from_str(response)?;
    Ok(
        matches!(request.command, Mutation::ObserveStopped { attempt, epoch: e } if attempt == id && e == epoch)
            && stopped.id == id
            && stopped.epoch == epoch
            && stopped.node == s.authority.1
            && stopped.state == "stopped",
    )
}

fn check(checks: &mut Vec<Value>, name: &str, passed: bool) {
    checks.push(json!({"check":name,"status":if passed {"passed"}else{"retained"}}));
}

pub fn inspect(state: &Path, config: &ExecutorConfig, id: &str, epoch: u64) -> Result<Value> {
    inspect_with(state, config, id, epoch, &RealRunner)
}

pub(super) fn inspect_with(
    state: &Path,
    config: &ExecutorConfig,
    id: &str,
    epoch: u64,
    runner: &dyn Runner,
) -> Result<Value> {
    uuid::Uuid::parse_str(id)?;
    ensure!(epoch > 0, "expected epoch must be positive");
    let mut c = super::reception::local_db(state, true, false)?;
    c.execute_batch("PRAGMA query_only=ON;")?;
    let original = scope(&mut c, id, epoch)?;
    ensure!(original.job.0 == epoch, "attempt epoch changed");
    let order: ExecuteOrder = serde_json::from_str(&original.job.2)?;
    ensure!(
        order.attempt.id == id
            && order.attempt.node == original.authority.1
            && order.attempt.epoch <= epoch,
        "assignment scope changed"
    );
    let mut checks = Vec::new();
    check(&mut checks, "node_stopped", original.job.1 == "stopped");
    check(
        &mut checks,
        "controller_stop_applied",
        applied_stop(&original, id, epoch).unwrap_or(false),
    );
    let (plan, never_launched) = match serde_json::from_str::<CheckoutPlan>(&original.job.3) {
        Ok(plan) => (plan, true),
        Err(_) => {
            let launch: LaunchPlan =
                serde_json::from_str(&original.job.3).context("no durable checkout plan")?;
            let never = !launch.launch_started && !launch.wrapper_started;
            (launch.checkout, never)
        }
    };
    check(
        &mut checks,
        "no_native_launch_intent",
        never_launched && original.process.is_none(),
    );
    ensure!(
        config.herdr_socket == plan.socket
            && config.herdr_binary == plan.herdr_binary
            && plan.socket.is_absolute()
            && std::fs::canonicalize(
                config
                    .repositories
                    .get(&order.repo)
                    .context("repository authorization missing")?
            )? == plan.repo
            && std::fs::canonicalize(&config.worktree_root)?.join(id) == plan.worktree
            && plan.branch == format!("hs/{}/{id}", order.attempt.run)
            && plan.base_oid == order.attempt.base_oid,
        "checkout ownership or executor configuration changed"
    );
    oid(&plan.base_oid)?;
    check(
        &mut checks,
        "creation_observed",
        plan.creation_started && plan.binding.is_some(),
    );
    let identity = git_identity(runner, &plan);
    check(&mut checks, "git_checkout_identity", identity.is_ok());
    let mut head = Value::Null;
    if let Ok(current) = identity {
        head = json!(current);
        let status = git(
            runner,
            &plan.worktree,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignored=matching",
                "--ignore-submodules=none",
            ],
        );
        check(
            &mut checks,
            "tracked_and_untracked_clean",
            status.as_ref().is_ok_and(|s| {
                s.split('\0')
                    .filter(|s| !s.is_empty())
                    .all(|s| s.starts_with("!! "))
            }),
        );
        check(
            &mut checks,
            "no_ignored_files",
            status
                .as_ref()
                .is_ok_and(|s| !s.split('\0').any(|s| s.starts_with("!! "))),
        );
        let flags = git(runner, &plan.worktree, &["ls-files", "-v", "-z"]);
        check(
            &mut checks,
            "no_hidden_index_entries",
            flags.is_ok_and(|s| {
                s.split('\0')
                    .filter(|s| !s.is_empty())
                    .all(|s| s.starts_with("H "))
            }),
        );
        let entries = git(runner, &plan.worktree, &["ls-files", "--stage", "-z"]);
        check(
            &mut checks,
            "no_submodules",
            entries.is_ok_and(|s| !s.split('\0').any(|s| s.starts_with("160000 "))),
        );
    }
    // Native responses/errors may contain terminal contents: never copy them to
    // the report. Missing saved binding cannot be repaired by adopting a pane.
    let native_ok = if plan.binding.is_some() {
        super::executor::observe_pane(&Herdr::new(&plan.herdr_binary, &plan.socket, runner), &plan)
            .is_ok()
    } else {
        false
    };
    check(&mut checks, "exact_idle_foreground_pane", native_ok);
    checks.push(json!({"check":"descendants_and_background_processes_exited","status":"unknown"}));
    checks.push(json!({"check":"retention_policy_satisfied","status":"unknown"}));
    check(
        &mut checks,
        "authority_unchanged",
        scope(&mut c, id, epoch)? == original,
    );
    let reasons: Vec<_> = checks
        .iter()
        .filter(|v| v["status"] != "passed")
        .map(|v| v["check"].clone())
        .collect();
    Ok(
        json!({"attempt":id,"epoch":epoch,"node":original.authority.1,"worktree":plan.worktree,
        "branch":plan.branch,"head_oid":head,"checks":checks,"retention_reasons":reasons,
        "removal_authorized":false,"resources_deleted":false,"observed_at":super::store::now(),
        "consistency":"independent observations with local authority recheck; not an atomic removal permit"}),
    )
}

fn git_identity(runner: &dyn Runner, plan: &CheckoutPlan) -> Result<String> {
    ensure!(
        std::fs::canonicalize(&plan.worktree)? == plan.worktree
            && std::fs::symlink_metadata(plan.worktree.join(".git"))?.is_file(),
        "not the original linked checkout"
    );
    for repo in [&plan.repo, &plan.worktree] {
        let common = git(
            runner,
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        ensure!(
            std::fs::canonicalize(common)? == plan.common_dir,
            "repository identity changed"
        );
    }
    ensure!(
        git(runner, &plan.worktree, &["symbolic-ref", "--quiet", "HEAD"])?
            == format!("refs/heads/{}", plan.branch),
        "branch changed"
    );
    let head = git(runner, &plan.worktree, &["rev-parse", "HEAD"])?;
    oid(&head)?;
    Ok(head)
}
