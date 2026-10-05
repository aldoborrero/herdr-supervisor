use super::*;
use crate::control::{lifecycle, node::NodeStore, runtime::RuntimeConfig};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
fn process_wrapper_child() {
    let Ok(state) = std::env::var("HS_PROCESS_STATE") else {
        return;
    };
    let attempt = std::env::var("HS_PROCESS_ATTEMPT").unwrap();
    let deadline = std::env::var("HS_PROCESS_DEADLINE")
        .unwrap()
        .parse()
        .unwrap();
    let mut command = std::process::Command::new(
        std::env::var("HS_PROCESS_BINARY").unwrap_or_else(|_| "sh".into()),
    );
    command.args(["-c", &std::env::var("HS_PROCESS_SCRIPT").unwrap()]);
    let state = std::path::Path::new(&state);
    let lease = crate::control::api::lock(state, "execution.lock").unwrap();
    let c = rusqlite::Connection::open(state.join("node.sqlite")).unwrap();
    c.busy_timeout(Duration::from_secs(2)).unwrap();
    c.execute_batch("PRAGMA synchronous=FULL").unwrap();
    crate::control::process::run(c, lease, &attempt, 1, deadline, command).unwrap();
    if std::env::var_os("HS_PROCESS_PTY").is_some() {
        assert_eq!(
            unsafe { libc::tcgetpgrp(0) },
            unsafe { libc::getpgrp() },
            "wrapper must restore foreground group"
        );
    }
}

struct ProcessFixture {
    f: Fixture,
    node: NodeStore,
    attempt: Attempt,
    worker: String,
    config: RuntimeConfig,
    marker: PathBuf,
}
impl ProcessFixture {
    fn new() -> Self {
        let mut f = Fixture::new();
        let task = f.task();
        let (attempt, worker) = f.dispatch(&task);
        f.ready(&attempt, &worker);
        let mut node = node_store(&f);
        let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
        let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
        node.receive(&f.store.prepare_wire(&effect.id, &owner).unwrap())
            .unwrap();
        let credential = node.dir.join("node.token");
        secret_file(&credential, f.node.as_bytes()).unwrap();
        let config = RuntimeConfig {
            supervisor_binary: "/unused".into(),
            mailer_state: "/unused".into(),
            mailer_admin_credential: "/unused".into(),
            node_credential: credential,
            codex_binary: "/unused".into(),
            mailer_runtime_binary: "/unused".into(),
            claude_development_channel: false,
        };
        let marker = f.dir.path().join("child-marker");
        Self {
            f,
            node,
            attempt,
            worker,
            config,
            marker,
        }
    }
    fn command(&self, script: &str, deadline: i64) -> std::process::Command {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "control::tests::lifecycle_tests::process_wrapper_child",
            "--nocapture",
        ])
        .env("HS_PROCESS_STATE", &self.node.dir)
        .env("HS_PROCESS_ATTEMPT", &self.attempt.id)
        .env("HS_PROCESS_SCRIPT", script)
        .env("HS_PROCESS_DEADLINE", deadline.to_string())
        .env("HS_PROCESS_MARKER", &self.marker)
        .stdout(std::process::Stdio::null());
        cmd
    }
    fn wait_started(&self) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let running: bool = self
                .node
                .connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_processes WHERE state='running')",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            if running && self.marker.exists() {
                break;
            }
            assert!(Instant::now() < until, "child did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cancel(&mut self) {
        call(
            &mut self.f.store,
            &self.f.supervisor,
            Mutation::StopAttempt {
                attempt: self.attempt.id.clone(),
                expected_epoch: 1,
            },
        );
        let effect = self
            .f
            .store
            .pending(&self.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "cancel")
            .unwrap();
        let owner = Identity::load(&self.f.store.dir.join("owner.key")).unwrap();
        self.node
            .receive(&self.f.store.prepare_wire(&effect.id, &owner).unwrap())
            .unwrap();
    }
    fn receipt(&self) -> Value {
        let doc: String = self
            .node
            .connection
            .query_row(
                "SELECT doc FROM runtime_processes WHERE attempt=?1 AND state='exited'",
                [&self.attempt.id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&doc).unwrap()
    }
    fn active(&self) -> i64 {
        self.f
            .store
            .connection
            .query_row("SELECT count(*) FROM attempts WHERE active=1", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
    fn deliver_stop(&mut self) {
        for (_, event) in self.node.pending().unwrap() {
            self.f.store.receive(&event).unwrap();
        }
    }
}

#[test]
fn normal_exit_receipt_survives_restart_and_loss_between_event_and_job_update() {
    let mut x = ProcessFixture::new();
    call(
        &mut x.f.store,
        &x.worker,
        Mutation::SubmitResult {
            attempt: x.attempt.id.clone(),
            epoch: 1,
            contract_hash: x.attempt.contract_hash.clone(),
            commit: Some(BASE.into()),
            artifacts: BTreeMap::new(),
            summary: "done".into(),
        },
    );
    let mut child=WrapperChild(x.command(": > \"$HS_PROCESS_MARKER\"; while [ ! -f \"$HS_PROCESS_MARKER.release\" ]; do sleep 0.02; done; exit 7",now()+30).spawn().unwrap());
    x.wait_started();
    assert_eq!(x.active(), 1);
    assert_eq!(
        lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap()["observed"],
        json!([])
    );
    std::fs::write(format!("{}.release", x.marker.display()), b"done").unwrap();
    assert!(child.wait().success());
    assert_eq!(x.receipt()["code"], 7);
    assert_eq!(x.active(), 1, "exit is not yet a controller receipt");
    x.node = node_store(&x.f);
    x.node.connection.execute_batch("CREATE TRIGGER fail_stop BEFORE UPDATE ON jobs WHEN NEW.state='stopped' BEGIN SELECT RAISE(ABORT,'injected receipt loss'); END;").unwrap();
    assert!(lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).is_err());
    assert_eq!(x.node.pending().unwrap().len(), 1);
    x.node
        .connection
        .execute_batch("DROP TRIGGER fail_stop")
        .unwrap();
    lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
    assert_eq!(
        x.node.pending().unwrap().len(),
        1,
        "reconciliation must reuse the same signed event"
    );
    x.deliver_stop();
    assert_eq!(x.active(), 0);
    assert_eq!(
        document::<Task>(&x.f.store.connection, "tasks", &x.attempt.task)
            .unwrap()
            .state,
        "awaiting_review"
    );
}

#[test]
fn cancellation_stops_owned_child_and_fences_workers_before_releasing_capacity() {
    for ignores_term in [false, true] {
        let mut x = ProcessFixture::new();
        let script = if ignores_term {
            "trap '' TERM; : > \"$HS_PROCESS_MARKER\"; while :; do sleep 0.05; done"
        } else {
            ": > \"$HS_PROCESS_MARKER\"; while :; do sleep 0.05; done"
        };
        let mut child = WrapperChild(x.command(script, now() + 30).spawn().unwrap());
        x.wait_started();
        x.cancel();
        assert_eq!(x.active(), 1);
        assert!(
            x.f.store
                .apply(&request(
                    &x.worker,
                    Mutation::ProposeMemory {
                        run: x.f.run.clone(),
                        topic: "late".into(),
                        text: "stale".into(),
                        source_attempt: Some(x.attempt.id.clone()),
                        evidence: vec!["claim".into()]
                    }
                ))
                .is_err()
        );
        assert!(child.wait().success());
        assert_eq!(x.receipt()["reason"], "cancelled");
        assert_eq!(x.receipt()["kill_escalated"], ignores_term);
        lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
        x.deliver_stop();
        assert_eq!(x.active(), 0);
        assert_eq!(
            document::<Task>(&x.f.store.connection, "tasks", &x.attempt.task)
                .unwrap()
                .state,
            "interrupted"
        );
    }
}

#[test]
fn deadline_stops_native_process_without_a_live_controller_and_controller_expires_run() {
    let mut x = ProcessFixture::new();
    let mut child = WrapperChild(
        x.command(
            ": > \"$HS_PROCESS_MARKER\"; while :; do sleep 0.02; done",
            now() + 2,
        )
        .spawn()
        .unwrap(),
    );
    x.wait_started();
    assert!(child.wait().success());
    assert_eq!(x.receipt()["reason"], "deadline");
    let mut run: Run = document(&x.f.store.connection, "runs", &x.f.run).unwrap();
    run.deadline = now() - 1;
    save_run(&x.f.store.connection, &run).unwrap();
    assert_eq!(lifecycle::expire_runs(&mut x.f.store).unwrap(), 1);
    assert_eq!(lifecycle::expire_runs(&mut x.f.store).unwrap(), 0);
    let effect =
        x.f.store
            .pending(&x.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "cancel")
            .unwrap();
    let owner = Identity::load(&x.f.store.dir.join("owner.key")).unwrap();
    x.node
        .receive(&x.f.store.prepare_wire(&effect.id, &owner).unwrap())
        .unwrap();
    lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
    x.deliver_stop();
    assert_eq!(
        document::<Run>(&x.f.store.connection, "runs", &x.f.run)
            .unwrap()
            .state,
        "cancelled"
    );
}

#[test]
fn missing_exit_receipt_never_signals_a_saved_pid_or_frees_capacity() {
    let mut x = ProcessFixture::new();
    let mut unrelated = WrapperChild(
        std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    );
    let doc = json!({"child_pid":unrelated.0.id(),"wrapper_pid":999999,"scope":"native_process"});
    x.node
        .connection
        .execute(
            "INSERT INTO runtime_processes VALUES (?1,1,'running',?2)",
            rusqlite::params![x.attempt.id, doc.to_string()],
        )
        .unwrap();
    x.cancel();
    x.node = node_store(&x.f);
    assert_eq!(
        lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap()["observed"],
        json!([])
    );
    assert_eq!(x.active(), 1);
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn spawn_failure_and_wrapper_signal_have_durable_terminal_receipts() {
    let mut x = ProcessFixture::new();
    let mut child = WrapperChild(
        x.command("", now() + 30)
            .env(
                "HS_PROCESS_BINARY",
                "/definitely-missing-herdr-test-runtime",
            )
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(!child.wait().success());
    assert_eq!(x.receipt()["reason"], "spawn_failed");
    lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
    x.deliver_stop();
    assert_eq!(x.active(), 0);
    let mut x = ProcessFixture::new();
    let mut child = WrapperChild(
        x.command(
            ": > \"$HS_PROCESS_MARKER\"; while :; do sleep 0.02; done",
            now() + 30,
        )
        .spawn()
        .unwrap(),
    );
    x.wait_started();
    unsafe {
        libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM);
    }
    assert!(child.wait().success());
    assert_eq!(x.receipt()["reason"], "wrapper_signal");
    lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
    x.deliver_stop();
    assert_eq!(x.active(), 0);
}

#[test]
fn node_v2_migration_preserves_pending_events_and_does_not_invent_process_receipts() {
    let mut x = ProcessFixture::new();
    let req = request(
        &x.worker,
        Mutation::WorkerReady {
            attempt: x.attempt.id.clone(),
            epoch: 1,
            session: x.attempt.native_session.clone().unwrap_or_else(id),
        },
    );
    let event = x.node.enqueue(&req, &x.f.node_key).unwrap();
    let old = serde_json::to_value(x.node.pending().unwrap()).unwrap();
    x.node
        .connection
        .execute_batch("ALTER TABLE events DROP COLUMN tries; ALTER TABLE events DROP COLUMN available_at; ALTER TABLE events DROP COLUMN last_error; DROP TABLE memory_notices; DROP TABLE worker_memory; DROP TABLE runtime_processes; DROP TABLE artifact_staging; DROP TABLE reception_failures; PRAGMA user_version=2;")
        .unwrap();
    x.node = node_store(&x.f);
    assert_eq!(
        serde_json::to_value(x.node.pending().unwrap()).unwrap(),
        old
    );
    assert_eq!(x.node.enqueue(&req, &x.f.node_key).unwrap(), event);
    let count: i64 = x
        .node
        .connection
        .query_row("SELECT count(*) FROM runtime_processes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn cancellation_before_launch_is_provable_and_late_execute_stays_fenced() {
    let mut x = ProcessFixture::new();
    // Start a second isolated copy of the node before it receives the execute order.
    let fresh = x.f.dir.path().join("late-node");
    x.node = NodeStore::open(
        &fresh,
        "local",
        &x.f.store.owner().unwrap(),
        x.node.grant.clone(),
        &x.f.node_key,
    )
    .unwrap();
    x.cancel();
    lifecycle::reconcile(&mut x.node, &x.config, &x.f.node_key).unwrap();
    x.deliver_stop();
    assert_eq!(x.active(), 0);
    let n: i64 = x
        .node
        .connection
        .query_row("SELECT count(*) FROM runtime_processes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let effect =
        x.f.store
            .pending(&x.f.node, "local")
            .unwrap()
            .into_iter()
            .find(|e| e.kind == "execute")
            .unwrap();
    let owner = Identity::load(&x.f.store.dir.join("owner.key")).unwrap();
    x.node
        .receive(&x.f.store.prepare_wire(&effect.id, &owner).unwrap())
        .unwrap();
    let state: String = x
        .node
        .connection
        .query_row("SELECT state FROM jobs WHERE id=?1", [&x.attempt.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        state, "stopped",
        "a late execute must not undo the terminal fence"
    );
}

#[test]
fn interactive_native_child_gets_foreground_terminal_and_wrapper_restores_it() {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    let x = ProcessFixture::new();
    let (mut master, mut slave) = (0, 0);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut cmd = x.command(
        "test -t 0 || exit 9; : > \"$HS_PROCESS_MARKER\"; read -r answer; test \"$answer\" = hello",
        now() + 30,
    );
    cmd.env("HS_PROCESS_PTY", "1")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = WrapperChild(cmd.spawn().unwrap());
    x.wait_started();
    master.write_all(b"hello\n").unwrap();
    assert!(child.wait().success());
    assert_eq!(x.receipt()["code"], 0);
}
