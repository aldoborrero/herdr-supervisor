use super::*;
use crate::{
    control::{
        cleanup,
        executor::{self, CheckoutPlan, ExecutorConfig},
        node::NodeStore,
    },
    runner::{Cmd, Output, RealRunner, Runner},
};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    time::Duration,
};

#[path = "retirement_tests.rs"]
mod retirement_tests;

fn git(path: &Path, args: &[&str]) -> String {
    let out = RealRunner
        .run(
            &Cmd::new("git", Duration::from_secs(10))
                .cwd(path)
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args.iter().copied()),
        )
        .unwrap();
    assert!(out.success(), "{}", out.error_text());
    out.stdout.trim().into()
}

struct Native {
    path: PathBuf,
    terminal: Cell<u32>,
    foreground: Cell<bool>,
    calls: Cell<u32>,
    change_database: Option<PathBuf>,
    removal: Cell<u8>,
    remove_calls: Cell<u32>,
    resources_absent: Cell<bool>,
}
impl Runner for Native {
    fn run(&self, cmd: &Cmd) -> anyhow::Result<Output> {
        if cmd.program != "fake-herdr" {
            return RealRunner.run(cmd);
        }
        self.calls.set(self.calls.get() + 1);
        let has = |s: &str| cmd.args.iter().any(|v| v == s);
        let result = if has("create") {
            let at = |s: &str| &cmd.args[cmd.args.iter().position(|v| v == s).unwrap() + 1];
            git(
                Path::new(at("--cwd")),
                &[
                    "worktree",
                    "add",
                    "-b",
                    at("--branch"),
                    at("--path"),
                    at("--base"),
                ],
            );
            return Ok(Output {
                timed_out: true,
                ..Default::default()
            });
        } else if has("remove") {
            assert!(!has("--force"));
            self.remove_calls.set(self.remove_calls.get() + 1);
            assert!(self.removal.get() > 0, "unexpected retirement");
            if self.removal.get() != 3 {
                git(
                    &self.path,
                    &["worktree", "remove", self.path.to_str().unwrap()],
                );
                if self.removal.get() != 4 {
                    self.resources_absent.set(true);
                }
            }
            return Ok(Output {
                code: Some(0),
                timed_out: self.removal.get() != 1,
                stdout: "{}".into(),
                ..Default::default()
            });
        } else if has("process-info") {
            if let Some(db) = &self.change_database {
                rusqlite::Connection::open(db)?.execute("UPDATE jobs SET epoch=epoch+1", [])?;
            }
            json!({"process_info":{"shell_pid":1,"foreground_process_group_id":1,"foreground_processes":if self.foreground.get() { json!([{"pid":1}]) }else{ json!([]) }}})
        } else if has("workspace") && has("list") {
            json!({"workspaces":if self.resources_absent.get() {json!([])} else {json!([{"workspace_id":"w1","worktree":{"checkout_path":self.path,"is_linked_worktree":true}}])}})
        } else if has("pane") && has("list") {
            json!({"panes":if self.resources_absent.get() {json!([])} else {json!([{"workspace_id":"w1","tab_id":"t1","pane_id":"p1","terminal_id":format!("term{}",self.terminal.get()),"cwd":self.path}])}})
        } else if has("agent") && has("list") {
            json!({"agents":[]})
        } else {
            panic!("unexpected native mutation: {}", cmd.display());
        };
        Ok(Output {
            code: Some(0),
            stdout: json!({"result":result}).to_string(),
            ..Default::default()
        })
    }
    fn socket_request(&self, _: &Path, _: &str, _: Duration) -> anyhow::Result<String> {
        panic!("no sockets")
    }
    fn run_foreground(&self, _: &Cmd, _: &mut dyn FnMut() -> bool) -> anyhow::Result<Option<i32>> {
        panic!("no models")
    }
}

struct Setup {
    f: Fixture,
    node: NodeStore,
    attempt: Attempt,
    config: ExecutorConfig,
    native: Native,
    plan: CheckoutPlan,
}
impl Setup {
    fn new() -> Self {
        let mut f = Fixture::new();
        let repo = f.dir.path().join("repo with spaces");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        std::fs::write(repo.join("tracked"), "base\n").unwrap();
        std::fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        run.base_oid = base.clone();
        run.integration_oid = base;
        save_run(&f.store.connection, &run).unwrap();
        let task = f.task();
        let (attempt, _) = f.dispatch(&task);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let event = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&event.id, &owner).unwrap())
            .unwrap();
        let root = f.dir.path().join("worktrees");
        let config = ExecutorConfig {
            repositories: BTreeMap::from([("/repo".into(), repo)]),
            base_remotes: BTreeMap::new(),
            worktree_root: root.clone(),
            herdr_binary: "fake-herdr".into(),
            herdr_socket: f.dir.path().join("herdr.sock"),
        };
        let native = Native {
            path: root.join(&attempt.id),
            terminal: Cell::new(1),
            foreground: Cell::new(true),
            calls: Cell::new(0),
            change_database: None,
            removal: Cell::new(0),
            remove_calls: Cell::new(0),
            resources_absent: Cell::new(false),
        };
        let plan = executor::prepare(&mut node, &config, &attempt.id, &native).unwrap();
        Self {
            f,
            node,
            attempt,
            config,
            native,
            plan,
        }
    }
    fn inspect(&self) -> Value {
        cleanup::inspect_with(
            &self.node.dir,
            &self.config,
            &self.attempt.id,
            self.attempt.epoch,
            &self.native,
        )
        .unwrap()
    }
    fn stop(&mut self) {
        let req = Request {
            token: self.f.node.clone(),
            operation_id: format!("stopped-{}-{}", self.attempt.id, self.attempt.epoch),
            command: Mutation::ObserveStopped {
                attempt: self.attempt.id.clone(),
                epoch: self.attempt.epoch,
            },
        };
        self.node.enqueue(&req, &self.f.node_key).unwrap();
        let (_, event) = self.node.pending().unwrap().remove(0);
        self.f.store.receive(&event).unwrap();
        let receipt = self
            .f
            .store
            .pending(&self.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "receipt")
            .unwrap();
        let owner = Identity::load(&self.f.store.dir.join("owner.key")).unwrap();
        self.node
            .receive(&self.f.store.prepare_wire(&receipt.id, &owner).unwrap())
            .unwrap();
        self.node
            .connection
            .execute("UPDATE jobs SET state='stopped'", [])
            .unwrap();
    }
}
fn status(report: &Value, name: &str) -> String {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["check"] == name)
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn cleanup_inspection_preserves_clean_checkout_refs_and_authority_even_after_stop() {
    let mut s = Setup::new();
    s.stop();
    let before: String = s
        .node
        .connection
        .query_row("SELECT doc||checkpoint FROM jobs", [], |r| r.get(0))
        .unwrap();
    let report = s.inspect();
    for name in [
        "node_stopped",
        "controller_stop_applied",
        "git_checkout_identity",
        "no_ignored_files",
        "tracked_and_untracked_clean",
        "no_hidden_index_entries",
        "no_submodules",
        "exact_idle_foreground_pane",
        "authority_unchanged",
    ] {
        assert_eq!(status(&report, name), "passed", "{name}: {report}");
    }
    assert_eq!(report["removal_authorized"], false);
    assert_eq!(
        status(&report, "descendants_and_background_processes_exited"),
        "unknown"
    );
    assert!(s.plan.worktree.exists());
    assert_eq!(
        git(&s.plan.repo, &["rev-parse", &s.plan.branch]),
        s.plan.base_oid
    );
    assert_eq!(
        before,
        s.node
            .connection
            .query_row("SELECT doc||checkpoint FROM jobs", [], |r| r
                .get::<_, String>(0))
            .unwrap()
    );
    assert!(!report.to_string().contains(&s.f.node));
    drop(s.node);
    let node = node_store(&s.f);
    assert!(
        cleanup::inspect_with(
            &node.dir,
            &s.config,
            &s.attempt.id,
            s.attempt.epoch,
            &s.native
        )
        .is_ok()
    );
}

#[test]
fn cleanup_inspection_detects_ignored_untracked_modified_and_hidden_index_entries() {
    let s = Setup::new();
    let dir = s.plan.worktree.join("ignored");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("private-token"), "do not echo this secret").unwrap();
    let r = s.inspect();
    assert_eq!(status(&r, "no_ignored_files"), "retained");
    assert_eq!(status(&r, "tracked_and_untracked_clean"), "passed");
    assert!(!r.to_string().contains("private-token"));
    std::fs::write(s.plan.worktree.join("new\nname"), "new").unwrap();
    assert_eq!(
        status(&s.inspect(), "tracked_and_untracked_clean"),
        "retained"
    );
    std::fs::remove_file(s.plan.worktree.join("new\nname")).unwrap();
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        git(&s.plan.worktree, &["update-index", flag, "tracked"]);
        std::fs::write(s.plan.worktree.join("tracked"), "hidden changes").unwrap();
        assert_eq!(status(&s.inspect(), "no_hidden_index_entries"), "retained");
        git(
            &s.plan.worktree,
            &["update-index", "--no-assume-unchanged", "tracked"],
        );
        git(
            &s.plan.worktree,
            &["update-index", "--no-skip-worktree", "tracked"],
        );
        assert_eq!(
            status(&s.inspect(), "tracked_and_untracked_clean"),
            "retained"
        );
        git(&s.plan.worktree, &["restore", "tracked"]);
    }
}

#[test]
fn cleanup_inspection_fences_changed_epoch_configuration_branch_and_symlink() {
    let mut s = Setup::new();
    let before = s.native.calls.get();
    assert!(
        cleanup::inspect_with(
            &s.node.dir,
            &s.config,
            &s.attempt.id,
            s.attempt.epoch + 1,
            &s.native
        )
        .is_err()
    );
    s.config.herdr_socket = s.f.dir.path().join("foreign.sock");
    assert!(
        cleanup::inspect_with(
            &s.node.dir,
            &s.config,
            &s.attempt.id,
            s.attempt.epoch,
            &s.native
        )
        .is_err()
    );
    assert_eq!(s.native.calls.get(), before);
    s.config.herdr_socket = s.plan.socket.clone();
    git(&s.plan.worktree, &["switch", "--detach"]);
    assert_eq!(status(&s.inspect(), "git_checkout_identity"), "retained");
    git(&s.plan.worktree, &["switch", &s.plan.branch]);
    let moved = s.plan.worktree.with_extension("retained");
    std::fs::rename(&s.plan.worktree, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &s.plan.worktree).unwrap();
    assert_eq!(status(&s.inspect(), "git_checkout_identity"), "retained");
    assert!(moved.join("tracked").exists());
}

#[test]
fn cleanup_inspection_retains_gitlinks_and_does_not_accept_a_foreign_stop_receipt() {
    let mut s = Setup::new();
    s.stop();
    git(
        &s.plan.worktree,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},embedded", s.plan.base_oid),
        ],
    );
    assert_eq!(status(&s.inspect(), "no_submodules"), "retained");
    s.node
        .connection
        .execute(
            "UPDATE events SET response=json_set(response,'$.node','another-node')",
            [],
        )
        .unwrap();
    assert_eq!(status(&s.inspect(), "controller_stop_applied"), "retained");
}

#[test]
fn cleanup_inspection_does_not_promote_broker_receipts_process_exit_or_incomplete_native_observations()
 {
    let mut s = Setup::new();
    s.stop();
    s.node
        .connection
        .execute("UPDATE events SET state='broker_stored'", [])
        .unwrap();
    s.node
        .connection
        .execute(
            "INSERT INTO runtime_processes VALUES (?1,?2,'exited','{}')",
            rusqlite::params![s.attempt.id, s.attempt.epoch],
        )
        .unwrap();
    let r = s.inspect();
    assert_eq!(status(&r, "controller_stop_applied"), "retained");
    assert_eq!(status(&r, "no_native_launch_intent"), "retained");
    assert_eq!(
        status(&r, "descendants_and_background_processes_exited"),
        "unknown"
    );
    s.native.foreground.set(false);
    assert_eq!(
        status(&s.inspect(), "exact_idle_foreground_pane"),
        "retained"
    );
    s.native.foreground.set(true);
    s.native.terminal.set(2);
    assert_eq!(
        status(&s.inspect(), "exact_idle_foreground_pane"),
        "retained"
    );
    s.native.terminal.set(1);
    s.native.change_database = Some(s.node.dir.join("node.sqlite"));
    assert_eq!(status(&s.inspect(), "authority_unchanged"), "retained");
}

#[test]
fn cleanup_inspection_does_not_initialize_missing_state_or_run_repository_fsmonitor() {
    let s = Setup::new();
    let missing = s.f.dir.path().join("missing");
    assert!(
        cleanup::inspect_with(
            &missing,
            &s.config,
            &s.attempt.id,
            s.attempt.epoch,
            &s.native
        )
        .is_err()
    );
    assert!(!missing.exists());
    let hook = s.f.dir.path().join("fsmonitor");
    std::fs::write(&hook, "#!/bin/sh\necho ran > \"$0.ran\"\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &s.plan.repo,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    assert_eq!(
        status(&s.inspect(), "tracked_and_untracked_clean"),
        "passed"
    );
    assert!(!hook.with_extension("ran").exists());
}
