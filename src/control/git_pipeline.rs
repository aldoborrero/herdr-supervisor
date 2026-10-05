//! Git/check executor for controller-owned integration intents. Only configured
//! repositories and owner-selected check argv are used. Worker claims are inputs,
//! never verification receipts.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::model::*;
use super::store::{Store, document, now, private_dir, save_run, save_task};
use super::verification::{current, gates, integration, save};
use crate::runner::{Cmd, Output, Runner};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub path: PathBuf,
    /// Explicit node -> Git remote mapping, supplied by the owner, never by a result.
    #[serde(default)]
    pub node_remotes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repositories: BTreeMap<String, Repository>,
    pub check_timeout_secs: u64,
}

pub fn read_config(path: &Path) -> Result<Config> {
    ensure!(
        std::fs::metadata(path)?.len() <= 262144,
        "verifier config too large"
    );
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        config.check_timeout_secs > 0 && config.check_timeout_secs <= 3600,
        "check timeout must be 1..3600 seconds"
    );
    Ok(config)
}

fn git_cmd(repo: &Path, args: &[&str]) -> Cmd {
    let mut cmd = Cmd::new("git", Duration::from_secs(30))
        .cwd(repo)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args.iter().copied())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .own_group();
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        cmd = cmd.env_remove(key);
    }
    cmd
}
fn git(runner: &dyn Runner, repo: &Path, args: &[&str]) -> Result<String> {
    let out = runner.run(&git_cmd(repo, args))?;
    ensure!(out.success(), "Git command failed: {}", out.error_text());
    Ok(out.stdout.trim_end().into())
}
fn common(runner: &dyn Runner, repo: &Path) -> Result<PathBuf> {
    Ok(std::fs::canonicalize(git(
        runner,
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?)
}
fn ref_value(runner: &dyn Runner, repo: &Path, name: &str) -> Result<Option<String>> {
    let out = runner.run(&git_cmd(repo, &["show-ref", "--verify", "--hash", name]))?;
    if out.success() {
        let value = out.stdout.trim().to_string();
        oid(&value)?;
        return Ok(Some(value));
    }
    // --exists distinguishes a missing reference (2) from a failed observation.
    let exists = runner.run(&git_cmd(repo, &["show-ref", "--exists", name]))?;
    ensure!(
        !exists.timed_out && exists.code == Some(2),
        "reference observation failed"
    );
    Ok(None)
}
fn detached_ref(runner: &dyn Runner, repo: &Path, name: &str) -> Result<()> {
    let symbolic = runner.run(&git_cmd(repo, &["symbolic-ref", "--quiet", name]))?;
    ensure!(
        !symbolic.timed_out && symbolic.code == Some(1),
        "owned integration reference must not be symbolic"
    );
    let worktrees = git(runner, repo, &["worktree", "list", "--porcelain", "-z"])?;
    ensure!(
        !worktrees.split('\0').any(|f| f == format!("branch {name}")),
        "integration branch is checked out; refusing to desynchronize its files"
    );
    Ok(())
}
fn pin(runner: &dyn Runner, repo: &Path, name: &str, value: &str) -> Result<()> {
    match ref_value(runner, repo, name)? {
        Some(current) => ensure!(current == value, "owned pin changed"),
        None => {
            git(
                runner,
                repo,
                &[
                    "update-ref",
                    "--no-deref",
                    name,
                    value,
                    &"0".repeat(value.len()),
                ],
            )?;
        }
    }
    Ok(())
}

fn binding(
    store: &mut Store,
    r: &Run,
    config: &Config,
    runner: &dyn Runner,
) -> Result<(PathBuf, String)> {
    let repo = std::fs::canonicalize(
        &config
            .repositories
            .get(&r.repo)
            .context("repository is not authorized for controller verification")?
            .path,
    )?;
    let dir = common(runner, &repo)?;
    let name = format!("refs/heads/hs/integration/{}", r.id);
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old: Option<(String, String, String, String, bool)> = tx
        .query_row(
            "SELECT path,common_dir,ref,state,active FROM repository_bindings WHERE run=?1",
            [&r.id],
            |v| Ok((v.get(0)?, v.get(1)?, v.get(2)?, v.get(3)?, v.get(4)?)),
        )
        .optional()?;
    let state = if let Some((path, common, reference, state, active)) = old {
        ensure!(
            active,
            "repository authority was released; this run cannot reacquire it"
        );
        ensure!(
            Path::new(&path) == repo && Path::new(&common) == dir && reference == name,
            "repository authorization changed for this run"
        );
        state
    } else {
        ensure!(
            ref_value(runner, &repo, &name)?.is_none(),
            "refusing to adopt an existing integration branch"
        );
        tx.execute(
            "INSERT INTO repository_bindings VALUES (?1,?2,?3,?4,'creating',1)",
            params![r.id, repo.to_string_lossy(), dir.to_string_lossy(), name],
        )?;
        "creating".into()
    };
    tx.commit()?;
    if state == "creating" {
        detached_ref(runner, &repo, &name)?;
        pin(runner, &repo, &name, &r.integration_oid)?;
        store.connection.execute(
            "UPDATE repository_bindings SET state='ready' WHERE run=?1",
            [&r.id],
        )?;
    }
    ensure!(
        state == "creating" || state == "ready",
        "repository binding is not ready"
    );
    Ok((repo, name))
}

fn journal(c: &Connection, i: &Integration, kind: &str) -> Result<()> {
    c.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('controller-verifier',?1,?2,?3,?4,?5)",params![format!("{}-{}-{}",kind,i.id,i.revision),i.id,kind,serde_json::to_string(i)?,now()])?;
    Ok(())
}
fn fail(store: &mut Store, i: &mut Integration, state: &str, message: &str) -> Result<()> {
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    i.state = state.into();
    i.error = Some(message.chars().take(4096).collect());
    i.revision += 1;
    save(&tx, i)?;
    let mut t: Task = document(&tx, "tasks", &i.task)?;
    if !["cancelled", "cancelling", "completed", "integrated"].contains(&t.state.as_str()) {
        t.state = "changes_requested".into();
        t.revision += 1;
        save_task(&tx, &t)?;
    }
    journal(&tx, i, state)?;
    tx.commit()?;
    Ok(())
}

fn exact_commit(runner: &dyn Runner, repo: &Path, commit: &str) -> Result<()> {
    oid(commit)?;
    ensure!(
        git(
            runner,
            repo,
            &["rev-parse", "--verify", &format!("{commit}^{{commit}}")]
        )? == commit,
        "result must be an exact commit object"
    );
    Ok(())
}

fn prepare(
    store: &mut Store,
    config: &Config,
    i: &mut Integration,
    repo: &Path,
    reference: &str,
    runner: &dyn Runner,
) -> Result<()> {
    let (r, _) = current(&store.connection, i)?;
    let a: Attempt = document(&store.connection, "attempts", &i.attempt)?;
    if ref_value(runner, repo, reference)?.as_deref() != Some(&i.base_oid)
        || r.integration_oid != i.base_oid
    {
        return fail(
            store,
            i,
            "stale_base",
            "integration base changed; prepare a new candidate and repeat its gates",
        );
    }
    if exact_commit(runner, repo, &i.result_oid).is_err() {
        let remote = config.repositories[&r.repo]
            .node_remotes
            .get(&a.node)
            .context("result object missing; configure an authorized remote for this node")?;
        let target = format!("{}:refs/herdr-supervisor/results/{}", i.result_oid, i.id);
        git(
            runner,
            repo,
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
    }
    exact_commit(runner, repo, &i.result_oid)?;
    exact_commit(runner, repo, &a.base_oid)?;
    let ancestor = runner.run(&git_cmd(
        repo,
        &["merge-base", "--is-ancestor", &a.base_oid, &i.result_oid],
    ))?;
    if ancestor.code == Some(1) && !ancestor.timed_out {
        return fail(
            store,
            i,
            "invalid_result",
            "result is not descended from its assigned base",
        );
    }
    ensure!(ancestor.success(), "result ancestry observation failed");
    pin(
        runner,
        repo,
        &format!("refs/herdr-supervisor/results/{}", i.id),
        &i.result_oid,
    )?;
    let merge = runner.run(&git_cmd(
        repo,
        &["merge-tree", "--write-tree", &i.base_oid, &i.result_oid],
    ))?;
    if merge.code == Some(1) && !merge.timed_out {
        return fail(store, i, "conflict", &merge.stdout);
    }
    ensure!(
        merge.success(),
        "candidate merge failed: {}",
        merge.error_text()
    );
    let tree = merge
        .stdout
        .lines()
        .next()
        .context("merge tree omitted object")?
        .to_string();
    oid(&tree)?;
    let timestamp = format!("@{} +0000", i.created_at);
    let commit = git_cmd(
        repo,
        &["commit-tree", &tree, "-p", &i.base_oid, "-p", &i.result_oid],
    )
    .env("GIT_AUTHOR_NAME", "Herdr Supervisor")
    .env("GIT_AUTHOR_EMAIL", "supervisor@local.invalid")
    .env("GIT_COMMITTER_NAME", "Herdr Supervisor")
    .env("GIT_COMMITTER_EMAIL", "supervisor@local.invalid")
    .env("GIT_AUTHOR_DATE", &timestamp)
    .env("GIT_COMMITTER_DATE", &timestamp)
    .stdin(format!(
        "Supervisor candidate {}\n\nContract {}\n",
        i.id, i.contract_hash
    ));
    let output = runner.run(&commit)?;
    ensure!(
        output.success(),
        "candidate commit failed: {}",
        output.error_text()
    );
    let candidate = output.stdout.trim().to_string();
    oid(&candidate)?;
    pin(
        runner,
        repo,
        &format!("refs/herdr-supervisor/candidates/{}", i.id),
        &candidate,
    )?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current_run: Run = document(&tx, "runs", &i.run)?;
    ensure!(
        current_run.state == "active" && now() < current_run.deadline,
        "run closed during candidate construction"
    );
    i.candidate_oid = Some(candidate);
    i.tree_oid = Some(tree);
    i.state = "checking".into();
    i.revision += 1;
    save(&tx, i)?;
    journal(&tx, i, "candidate_prepared")?;
    tx.commit()?;
    Ok(())
}

fn verify_checkout(runner: &dyn Runner, repo: &Path, path: &Path, commit: &str) -> Result<()> {
    ensure!(
        std::fs::canonicalize(path)? == path,
        "verification checkout path was substituted"
    );
    ensure!(
        common(runner, path)? == common(runner, repo)?,
        "verification checkout repository mismatch"
    );
    ensure!(
        git(runner, path, &["rev-parse", "HEAD"])? == commit,
        "verification checkout HEAD changed"
    );
    ensure!(
        git(
            runner,
            path,
            &["status", "--porcelain=v1", "--untracked-files=no"]
        )?
        .is_empty(),
        "verification changed tracked files"
    );
    let head = runner.run(&git_cmd(path, &["symbolic-ref", "--quiet", "HEAD"]))?;
    ensure!(
        !head.timed_out && head.code == Some(1),
        "verification checkout must stay detached"
    );
    Ok(())
}

fn checkout(
    store: &mut Store,
    i: &Integration,
    repo: &Path,
    runner: &dyn Runner,
) -> Result<PathBuf> {
    let parent = store.dir.join("verification");
    private_dir(&parent)?;
    let parent = parent.join(&i.id);
    private_dir(&parent)?;
    let path = parent.join("checkout");
    let old: Option<(String, String)> = store
        .connection
        .query_row(
            "SELECT path,state FROM verification_worktrees WHERE integration=?1",
            [&i.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((old_path, _)) = old {
        ensure!(Path::new(&old_path) == path, "verification path changed");
    } else {
        ensure!(
            !path.try_exists()?,
            "refusing to adopt pre-existing verification checkout"
        );
        store.connection.execute(
            "INSERT INTO verification_worktrees VALUES (?1,?2,'creating')",
            params![i.id, path.to_string_lossy()],
        )?;
        let _outcome = runner.run(&git_cmd(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str().context("verification path UTF8")?,
                i.candidate_oid.as_ref().context("candidate missing")?,
            ],
        ));
    }
    verify_checkout(
        runner,
        repo,
        &path,
        i.candidate_oid.as_ref().context("candidate missing")?,
    )?;
    store.connection.execute(
        "UPDATE verification_worktrees SET state='ready' WHERE integration=?1",
        [&i.id],
    )?;
    Ok(path)
}

fn check_evidence(output: &Output) -> Value {
    // Retain bounded readable excerpts and hashes of the complete captured output.
    let stdout = output.stdout.chars().take(16384).collect::<String>();
    let stderr = output.stderr.chars().take(16384).collect::<String>();
    json!({"stdout":stdout,"stderr":stderr,"stdout_hash":digest(output.stdout.as_bytes()),"stderr_hash":digest(output.stderr.as_bytes()),"truncated":stdout.len()!=output.stdout.len()||stderr.len()!=output.stderr.len(),"exit_code":output.code,"timed_out":output.timed_out})
}

fn executable(program: &str, cwd: &Path) -> Result<(PathBuf, String)> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let path = if program.contains('/') {
        cwd.join(program)
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| {
                if dir.is_absolute() {
                    dir.join(program)
                } else {
                    cwd.join(dir).join(program)
                }
            })
            .find(|path| {
                std::fs::metadata(path)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
            .context("check executable is not available")?
    };
    let path = std::fs::canonicalize(path)?;
    let mut file = std::fs::File::open(&path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16384];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok((path, format!("{:x}", hasher.finalize())))
}

const CHECK_ENV_REMOVE: &[&str] = &[
    "HERDR_MAILER_CREDENTIAL",
    "HERDR_SUPERVISOR_CREDENTIAL",
    "HERDR_MAILER_CONFIG",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
];

fn environment_hash() -> Result<String> {
    use std::os::unix::ffi::OsStrExt;
    let mut vars: Vec<(Vec<u8>, Vec<u8>)> = std::env::vars_os()
        .filter(|(key, _)| {
            key != "GIT_NO_REPLACE_OBJECTS" && !CHECK_ENV_REMOVE.iter().any(|s| key == *s)
        })
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
    vars.push((b"GIT_NO_REPLACE_OBJECTS".to_vec(), b"1".to_vec()));
    vars.sort();
    // Only a digest of the effective environment is retained, never its secrets.
    Ok(digest(&serde_json::to_vec(&vars)?))
}

fn checks(
    store: &mut Store,
    config: &Config,
    i: &mut Integration,
    repo: Option<&Path>,
    runner: &dyn Runner,
) -> Result<()> {
    let (r, t) = current(&store.connection, i)?;
    ensure!(
        r.state == "active" && now() < r.deadline,
        "run not active for checks"
    );
    // Recover historical outcomes before reobserving inputs. A check that dirtied
    // its checkout must become a known failure, not stay blocked on that dirtiness.
    for name in &t.contract.checks {
        let prior: Option<(String, String)> = store
            .connection
            .query_row(
                "SELECT id,state FROM check_runs WHERE integration=?1 AND name=?2",
                params![i.id, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((check, mut state)) = prior {
            if state == "started" {
                super::check_receipts::recover_one(store, &check)?;
                state = store.connection.query_row(
                    "SELECT state FROM check_runs WHERE id=?1",
                    [check],
                    |r| r.get(0),
                )?;
            }
            if state != "passed" {
                return fail(
                    store,
                    i,
                    "checks_failed",
                    "check outcome failed or uncertain; an explicit new verification is required",
                );
            }
        }
    }
    let path = if i.artifact.is_some() {
        super::artifact_inputs::controller(store, i)?
    } else {
        checkout(
            store,
            i,
            repo.context("code verifier repository missing")?,
            runner,
        )?
    };
    let candidate = super::verification::candidate_reference(i)?.context("candidate missing")?;
    let mut receipts = Vec::<Value>::new();
    for name in &t.contract.checks {
        let fresh: Run = document(&store.connection, "runs", &i.run)?;
        ensure!(
            fresh.state == "active" && now() < fresh.deadline,
            "run closed before next check"
        );
        let prior: Option<(String, String)> = store
            .connection
            .query_row(
                "SELECT state,doc FROM check_runs WHERE integration=?1 AND name=?2",
                params![i.id, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((state, doc)) = prior {
            if state != "passed" {
                return fail(
                    store,
                    i,
                    "checks_failed",
                    "check outcome failed or uncertain; an explicit new verification is required",
                );
            }
            receipts.push(serde_json::from_str(&doc)?);
            continue;
        }
        let argv = &r.policy.checks[name];
        let (program, program_hash) = executable(&argv[0], &path)?;
        super::check_receipts::prepare(&store.dir)?;
        let mut receipt = json!({"id":id(),"integration":i.id,"name":name,"candidate":candidate,"tree":i.tree_oid,"policy_hash":i.policy_hash,"argv":argv,"cwd":path,"program":program,"program_hash":program_hash,"environment_hash":environment_hash()?,"timeout_secs":config.check_timeout_secs,"started_at":now(),"receipt_scheme":super::check_receipts::SCHEME});
        let initial = receipt.clone();
        store.connection.execute(
            "INSERT INTO check_runs VALUES (?1,?2,?3,'started',?4)",
            params![
                receipt["id"].as_str(),
                i.id,
                name,
                serde_json::to_string(&receipt)?
            ],
        )?;
        let mut cmd = Cmd::new(
            program.to_str().context("check executable path UTF8")?,
            Duration::from_secs(config.check_timeout_secs),
        )
        .args(argv.iter().skip(1).cloned())
        .cwd(&path)
        .own_group()
        .env("GIT_NO_REPLACE_OBJECTS", "1");
        for key in CHECK_ENV_REMOVE {
            cmd = cmd.env_remove(*key);
        }
        let output = runner.run(&cmd)?;
        receipt["output"] = check_evidence(&output);
        receipt["finished_at"] = json!(now());
        let clean = if let Some(artifact) = &i.artifact {
            super::artifact_inputs::verify(&path, artifact)
        } else {
            verify_checkout(
                runner,
                repo.context("code verifier repository missing")?,
                &path,
                i.candidate_oid.as_ref().context("code candidate missing")?,
            )
        };
        let passed = output.success() && clean.is_ok();
        if let Err(error) = clean {
            receipt["checkout_error"] =
                json!(error.to_string().chars().take(4096).collect::<String>());
        }
        super::check_receipts::finish(store, &initial, receipt.clone(), passed)?;
        if !passed {
            return fail(
                store,
                i,
                "checks_failed",
                "check failed or mutated its candidate checkout",
            );
        }
        receipts.push(receipt);
    }
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (r, mut task) = current(&tx, i)?;
    ensure!(
        r.state == "active" && now() < r.deadline,
        "run closed during checks"
    );
    i.checks_digest = Some(digest(&serde_json::to_vec(&receipts)?));
    i.state = if r.policy.require_review {
        "awaiting_review"
    } else {
        "verified"
    }
    .into();
    i.revision += 1;
    save(&tx, i)?;
    task.state = i.state.clone();
    task.revision += 1;
    save_task(&tx, &task)?;
    journal(&tx, i, "checks_completed")?;
    tx.execute("UPDATE outbox SET state='applied' WHERE target='controller' AND kind='verify_candidate' AND json_extract(payload,'$.id')=?1",[&i.id])?;
    tx.commit()?;
    Ok(())
}

fn apply(
    store: &mut Store,
    i: &mut Integration,
    repo: &Path,
    reference: &str,
    runner: &dyn Runner,
) -> Result<()> {
    // The write transaction serializes the final gate/CAS/receipt window with
    // cancellation and replanning. A lost SQL commit is reconciled by exact ref.
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (mut r, mut t) = current(&tx, i)?;
    gates(&tx, i)?;
    let candidate = i.candidate_oid.as_ref().context("candidate missing")?;
    exact_commit(runner, repo, candidate)?;
    detached_ref(runner, repo, reference)?;
    let actual =
        ref_value(runner, repo, reference)?.context("owned integration branch disappeared")?;
    if &actual != candidate {
        ensure!(
            r.state == "active" && now() < r.deadline,
            "run closed before integration"
        );
        ensure!(
            actual == i.base_oid && r.integration_oid == i.base_oid,
            "integration base advanced; candidate requires new gates"
        );
        let result = runner.run(&git_cmd(
            repo,
            &[
                "update-ref",
                "--no-deref",
                reference,
                candidate,
                &i.base_oid,
            ],
        ))?;
        if !result.success() {
            ensure!(
                ref_value(runner, repo, reference)?.as_ref() == Some(candidate),
                "Git CAS not confirmed; integration intent retained"
            );
        }
    }
    ensure!(
        ref_value(runner, repo, reference)?.as_ref() == Some(candidate),
        "integration ref changed before receipt"
    );
    r.integration_oid = candidate.clone();
    r.revision += 1;
    save_run(&tx, &r)?;
    t.state = "integrated".into();
    t.revision += 1;
    save_task(&tx, &t)?;
    i.state = "applied".into();
    i.revision += 1;
    save(&tx, i)?;
    journal(&tx, i, "integration_applied")?;
    tx.execute("UPDATE outbox SET state='applied' WHERE target='controller' AND kind='integrate_candidate' AND json_extract(payload,'$.id')=?1",[&i.id])?;
    tx.commit()?;
    Ok(())
}

pub fn advance(
    store: &mut Store,
    config: &Config,
    id: &str,
    runner: &dyn Runner,
) -> Result<Integration> {
    ensure!(
        config.check_timeout_secs > 0 && config.check_timeout_secs <= 3600,
        "invalid check timeout"
    );
    let _lease = super::api::lock(&store.dir, "git-executor.lock")?;
    let mut i = integration(&store.connection, id)?;
    let (r, _) = current(&store.connection, &i)?;
    if i.state != "applying" {
        ensure!(
            r.state == "active" && now() < r.deadline,
            "run not active for verification"
        );
    }
    if i.artifact.is_some() {
        match i.state.as_str() {
            "preparing" => {
                let source: Attempt = document(&store.connection, "attempts", &i.attempt)?;
                ensure!(
                    i.artifact.as_ref()
                        == Some(&super::artifacts::candidate(&store.connection, &source)?),
                    "artifact candidate changed"
                );
                super::artifact_inputs::controller(store, &i)?;
                let tx = store
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (fresh, _) = current(&tx, &i)?;
                super::store::active_run(&fresh)?;
                i.state = "checking".into();
                i.revision += 1;
                save(&tx, &i)?;
                journal(&tx, &i, "artifact_candidate_prepared")?;
                tx.commit()?;
                checks(store, config, &mut i, None, runner)?;
            }
            "checking" => checks(store, config, &mut i, None, runner)?,
            "applying" => apply_artifact(store, &mut i)?,
            _ => (),
        }
        return Ok(i);
    }
    let (repo, reference) = binding(store, &r, config, runner)?;
    match i.state.as_str() {
        "preparing" => {
            prepare(store, config, &mut i, &repo, &reference, runner)?;
            if i.state == "checking" {
                checks(store, config, &mut i, Some(&repo), runner)?;
            }
        }
        "checking" => checks(store, config, &mut i, Some(&repo), runner)?,
        "applying" => apply(store, &mut i, &repo, &reference, runner)?,
        _ => (),
    }
    Ok(i)
}

fn apply_artifact(store: &mut Store, i: &mut Integration) -> Result<()> {
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (mut run, mut task) = current(&tx, i)?;
    if ["cancelling", "cancelled"].contains(&run.state.as_str()) {
        i.state = "superseded".into();
        i.revision += 1;
        save(&tx, i)?;
        journal(&tx, i, "artifact_acceptance_cancelled")?;
        tx.execute("UPDATE outbox SET state='cancelled' WHERE target='controller' AND kind='integrate_candidate' AND json_extract(payload,'$.id')=?1",[&i.id])?;
        tx.commit()?;
        return Ok(());
    }
    super::store::active_run(&run)?;
    gates(&tx, i)?;
    let artifact = i.artifact.as_ref().context("artifact candidate missing")?;
    let (path, state): (String, String) = tx.query_row(
        "SELECT path,state FROM verification_worktrees WHERE integration=?1",
        [&i.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let expected = store.dir.join("verification").join(&i.id).join("inputs");
    ensure!(
        Path::new(&path) == expected && state == "ready",
        "artifact inputs not ready or path changed"
    );
    super::artifact_inputs::verify(&expected, artifact)?;
    super::store::active_run(&run)?;
    task.state = "completed".into();
    task.revision += 1;
    save_task(&tx, &task)?;
    run.revision += 1;
    save_run(&tx, &run)?; // integration_oid deliberately remains the actual Git base.
    i.state = "applied".into();
    i.revision += 1;
    save(&tx, i)?;
    journal(&tx, i, "integration_applied")?;
    tx.execute("UPDATE outbox SET state='applied' WHERE target='controller' AND kind='integrate_candidate' AND json_extract(payload,'$.id')=?1",[&i.id])?;
    tx.commit()?;
    Ok(())
}
