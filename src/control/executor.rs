//! Node-local resource effects. Persist intent, then reconcile exact observations.
//! An uncertain mutation is never retried merely because its reply was lost.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::herdr::Herdr;
use crate::runner::{Cmd, Runner};

use super::model::{identifier, oid};
use super::node::{ExecuteOrder, NodeStore};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutorConfig {
    /// Controller repository identifiers map to explicitly authorized local checkouts.
    pub repositories: BTreeMap<String, PathBuf>,
    /// Owner-approved remotes for exact base/candidate objects absent on this node.
    #[serde(default)]
    pub base_remotes: BTreeMap<String, String>,
    pub worktree_root: PathBuf,
    pub herdr_binary: String,
    pub herdr_socket: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CheckoutPlan {
    pub repo: PathBuf,
    pub common_dir: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
    pub base_oid: String,
    pub socket: PathBuf,
    pub herdr_binary: String,
    pub creation_started: bool,
    pub binding: Option<PaneBinding>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PaneBinding {
    pub workspace: String,
    pub pane: String,
    pub terminal: String,
}

fn git(runner: &dyn Runner, repo: &Path, args: &[&str]) -> Result<String> {
    let out = runner.run(
        &Cmd::new("git", Duration::from_secs(20))
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args.iter().copied())
            .cwd(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_NAMESPACE")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_TERMINAL_PROMPT", "0"),
    )?;
    ensure!(
        out.success(),
        "Git observation failed: {}",
        out.error_text()
    );
    Ok(out.stdout.trim_end().into())
}

fn common_dir(runner: &dyn Runner, path: &Path) -> Result<PathBuf> {
    let value = git(
        runner,
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(std::fs::canonicalize(value)?)
}

pub(super) fn inspect_checkout(runner: &dyn Runner, plan: &CheckoutPlan) -> Result<()> {
    ensure!(
        std::fs::canonicalize(&plan.worktree)? == plan.worktree,
        "checkout path was substituted"
    );
    ensure!(
        common_dir(runner, &plan.worktree)? == plan.common_dir,
        "checkout repository changed"
    );
    ensure!(
        git(runner, &plan.worktree, &["symbolic-ref", "--quiet", "HEAD"])?
            == format!("refs/heads/{}", plan.branch),
        "checkout branch changed"
    );
    ensure!(
        git(runner, &plan.worktree, &["rev-parse", "HEAD"])? == plan.base_oid,
        "checkout base changed before launch"
    );
    ensure!(
        git(
            runner,
            &plan.worktree,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        )?
        .is_empty(),
        "checkout is not pristine before launch"
    );
    Ok(())
}

pub(super) fn observe_pane(herdr: &Herdr<'_>, plan: &CheckoutPlan) -> Result<PaneBinding> {
    let spaces = herdr.workspace_list()?;
    let matches: Vec<_> = spaces
        .iter()
        .filter(|space| {
            space.worktree.as_ref().is_some_and(|wt| {
                wt.is_linked_worktree && Path::new(&wt.checkout_path) == plan.worktree
            })
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "expected exactly one Herdr workspace for the checkout"
    );
    let workspace = &matches[0].workspace_id;
    let panes = herdr.pane_list()?;
    let matches: Vec<_> = panes
        .iter()
        .filter(|p| &p.workspace_id == workspace)
        .collect();
    ensure!(
        matches.len() == 1,
        "owned workspace must have exactly one pane before launch"
    );
    let pane = matches[0];
    ensure!(
        !pane.pane_id.is_empty() && !pane.terminal_id.is_empty(),
        "Herdr omitted exact pane identity"
    );
    ensure!(
        Path::new(&pane.cwd) == plan.worktree,
        "pane cwd does not match checkout"
    );
    ensure!(
        herdr
            .agent_list()?
            .iter()
            .all(|a| a.pane_id != pane.pane_id),
        "pane already contains an agent"
    );
    ensure!(
        herdr.pane_idle_shell(&pane.pane_id),
        "pane is not an idle shell"
    );
    let observed = PaneBinding {
        workspace: workspace.clone(),
        pane: pane.pane_id.clone(),
        terminal: pane.terminal_id.clone(),
    };
    if let Some(binding) = &plan.binding {
        ensure!(binding == &observed, "Herdr resource identity changed");
    }
    Ok(observed)
}

/// Preparation is useful independently of model launch and is safe to repeat.
/// The execution lease also fences incoming cancellation commands.
pub fn prepare(
    store: &mut NodeStore,
    config: &ExecutorConfig,
    attempt: &str,
    runner: &dyn Runner,
) -> Result<CheckoutPlan> {
    let _lease = super::api::lock(&store.dir, "execution.lock")?;
    store.grant.verify(jiff::Timestamp::now().as_second())?;
    identifier(attempt)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (epoch, state, doc, checkpoint): (u64, String, String, String) = tx.query_row(
        "SELECT epoch,state,doc,checkpoint FROM jobs WHERE id=?1",
        [attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    ensure!(
        ["reserved", "preparing", "prepared"].contains(&state.as_str()),
        "job is not eligible for preparation"
    );
    let order: ExecuteOrder = serde_json::from_str(&doc)?;
    ensure!(
        epoch == order.attempt.epoch && order.attempt.node == store.node_id,
        "attempt epoch or node changed"
    );
    identifier(&order.attempt.run)?;
    oid(&order.attempt.base_oid)?;
    let mut plan: CheckoutPlan = if state == "reserved" {
        let repo = std::fs::canonicalize(
            config
                .repositories
                .get(&order.repo)
                .context("repository is not authorized on this node")?,
        )?;
        let common_dir = common_dir(runner, &repo)?;
        super::store::private_dir(&config.worktree_root)?;
        let root = std::fs::canonicalize(&config.worktree_root)?;
        let worktree = root.join(attempt);
        ensure!(
            !worktree.try_exists()?,
            "refusing to adopt an existing checkout"
        );
        let branch = format!("hs/{}/{}", order.attempt.run, attempt);
        let existing = git(
            runner,
            &repo,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        ensure!(existing.is_empty(), "refusing to adopt an existing branch");
        let base_observation = git(
            runner,
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", order.attempt.base_oid),
            ],
        );
        let base = match base_observation {
            Ok(base) => base,
            Err(error) => {
                let Some(remote) = config.base_remotes.get(&order.repo) else {
                    return Err(error
                        .context("base object unavailable; configure an authorized base_remote"));
                };
                let target = format!(
                    "{}:refs/herdr-supervisor/inputs/{attempt}",
                    order.attempt.base_oid
                );
                git(
                    runner,
                    &repo,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--no-recurse-submodules",
                        "--",
                        remote,
                        &target,
                    ],
                )?;
                git(
                    runner,
                    &repo,
                    &[
                        "rev-parse",
                        "--verify",
                        &format!("{}^{{commit}}", order.attempt.base_oid),
                    ],
                )?
            }
        };
        ensure!(
            base == order.attempt.base_oid,
            "base object is not an exact commit"
        );
        ensure!(
            config.herdr_socket.is_absolute(),
            "Herdr socket must be absolute"
        );
        CheckoutPlan {
            repo,
            common_dir,
            worktree,
            branch,
            base_oid: base,
            socket: config.herdr_socket.clone(),
            herdr_binary: config.herdr_binary.clone(),
            creation_started: false,
            binding: None,
        }
    } else {
        serde_json::from_str(&checkpoint).context("invalid durable checkout plan")?
    };
    ensure!(
        config.herdr_socket == plan.socket
            && config.herdr_binary == plan.herdr_binary
            && std::fs::canonicalize(
                config
                    .repositories
                    .get(&order.repo)
                    .context("repository authorization removed")?
            )? == plan.repo
            && std::fs::canonicalize(&config.worktree_root)?.join(attempt) == plan.worktree,
        "executor configuration changed for an existing attempt"
    );
    tx.execute(
        "UPDATE jobs SET state='preparing',checkpoint=?2 WHERE id=?1",
        params![attempt, serde_json::to_string(&plan)?],
    )?;
    tx.commit()?;

    let herdr = Herdr::new(&plan.herdr_binary, &plan.socket, runner);
    if !plan.creation_started {
        // This marker survives a crash before, during or after the external command.
        // Reconciliation may prove success. Absence cannot prove the call never ran.
        plan.creation_started = true;
        store.connection.execute(
            "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
            params![attempt, serde_json::to_string(&plan)?],
        )?;
        let _outcome = herdr.call(
            &[
                "worktree",
                "create",
                "--cwd",
                plan.repo.to_str().context("non-UTF8 repository")?,
                "--branch",
                &plan.branch,
                "--base",
                &plan.base_oid,
                "--path",
                plan.worktree.to_str().context("non-UTF8 checkout")?,
                "--label",
                &format!("hs-{attempt}"),
                "--no-focus",
            ],
            Duration::from_secs(30),
        );
        // A successful reply is not sufficient either: independently inspect Git and Herdr.
    }
    inspect_checkout(runner, &plan)
        .context("checkout creation unresolved; no automatic duplicate creation")?;
    plan.binding = Some(observe_pane(&herdr, &plan)?);
    store.connection.execute(
        "UPDATE jobs SET state='prepared',checkpoint=?2 WHERE id=?1 AND epoch=?3",
        params![attempt, serde_json::to_string(&plan)?, epoch],
    )?;
    Ok(plan)
}

pub fn read_config(path: &Path) -> Result<ExecutorConfig> {
    ensure!(
        std::fs::metadata(path)?.len() <= 262144,
        "executor config too large"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

pub fn summary(plan: &CheckoutPlan) -> serde_json::Value {
    json!({"state":"prepared","worktree":plan.worktree,"branch":plan.branch,"base_oid":plan.base_oid,"binding":plan.binding,"model_ready":false})
}
