use super::*;
#[path = "coordinator_retirement_tests.rs"]
mod retirement_tests;
use crate::control::{coordinator, coordinator_launch as launch};
use crate::runner::fake::{ok, timeout};
use crate::scenarios::World;
use std::{
    io::{BufRead, Write},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) fn config(f: &Fixture) -> launch::Config {
    launch::Config {
        herdr_binary: "/unused/herdr".into(),
        herdr_socket: f.dir.path().join("herdr.sock"),
        supervisor_binary: "/unused/herdr-supervisor".into(),
        mailer_state: f.dir.path().join("mailer"),
        mailer_admin_credential: f.dir.path().join("mailer-admin.token"),
        codex_binary: "/unused/codex".into(),
        mailer_runtime_binary: "/unused/herdr-mailer-runtime".into(),
        claude_development_channel: false,
    }
}
fn start(f: &mut Fixture, config: launch::Config) -> String {
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    text(
        &call(
            &mut f.store,
            &f.owner,
            Mutation::StartCoordinator {
                run: f.run.clone(),
                expected_revision: run.revision,
                profile: "auto".into(),
                config,
            },
        ),
        "id",
    )
}
fn plan(f: &Fixture, id: &str) -> launch::Plan {
    let doc: String = f
        .store
        .connection
        .query_row(
            "SELECT doc FROM coordinator_launches WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&doc).unwrap()
}
fn runtime(f: &mut Fixture, name: &str) {
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    run.policy.profiles.get_mut("auto").unwrap().runtime = name.into();
    save_run(&f.store.connection, &run).unwrap();
}
fn shell(world: &World, home: &Path, lose_create: bool) {
    let panes = world.panes.clone();
    let home = home.to_path_buf();
    world.runner.on_fn(|c|c.display().contains("workspace create"),move |_| {
        *panes.borrow_mut()=json!([{"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1","terminal_id":"t1","cwd":home}]).to_string();
        Ok(if lose_create {timeout()}else{ok(r#"{"result":{"root_pane":{"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1"}}}"#)})
    });
    world.runner.on("pane process-info",ok(r#"{"result":{"process_info":{"shell_pid":100,"foreground_process_group_id":100,"foreground_processes":[{"pid":100}]}}}"#));
    world.runner.on("pane run", timeout());
}
fn observed(world: &World, p: &launch::Plan, uuid: &str, terminal: &str, home: &Path) {
    let session = if p.profile.runtime == "pi" {
        use std::os::unix::fs::PermissionsExt;
        let path = home.parent().unwrap().join("observed-pi.jsonl");
        let header = json!({"type":"session","version":3,"id":uuid,"cwd":home});
        std::fs::write(&path, format!("{header}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        json!({"kind":"path","value":path})
    } else {
        json!({"kind":"id","value":uuid})
    };
    *world.agents.borrow_mut()=json!([{"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1","terminal_id":terminal,"cwd":home,"agent":p.profile.runtime,"agent_session":session}]).to_string();
}
struct Mailer {
    lose_register: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    config: coordinator::Config,
}
impl Mailer {
    fn new(f: &Fixture, lose_register: bool) -> Self {
        let cfg = config(f);
        private_dir(&cfg.mailer_state).unwrap();
        secret_file(
            &cfg.mailer_admin_credential,
            b"isolated-admin-token-000000000000000000",
        )
        .unwrap();
        let sender = cfg.mailer_state.join("sender.token");
        secret_file(&sender, b"isolated-sender-token-00000000000000000").unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(cfg.mailer_state.join("daemon.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let lose_register = Arc::new(AtomicBool::new(lose_register));
        let lost = lose_register.clone();
        let (r, s) = (requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                let (mut socket, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                std::io::BufReader::new(socket.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let req: Value = serde_json::from_str(&line).unwrap();
                r.lock().unwrap().push(req.clone());
                let data = match req["op"].as_str().unwrap() {
                    "register" => {
                        if lost.load(Ordering::Relaxed) {
                            continue;
                        }
                        let epoch = r
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|v| v["op"] == "register")
                            .count();
                        json!({"session":{"id":id(),"agent":req["agent"],"runtime":req["runtime"],"location":req["location"],"epoch":epoch},"token":"isolated-receiver-token-000000000000000000"})
                    }
                    "capabilities" => json!({"idempotentSend":true,"recipientPrecondition":true}),
                    "doctor" => json!({"owner":"did:key:test","agent":"sender"}),
                    "send" => {
                        json!({"id":id(),"owner":"did:key:test","author":"sender","recipient":req["recipient"],"recipientSession":req["recipientSession"],"recipientEpoch":req["recipientEpoch"],"body":req["body"]})
                    }
                    op => panic!("unexpected {op}"),
                };
                writeln!(socket, "{}", json!({"data":data})).unwrap();
            }
        });
        Self {
            lose_register,
            requests,
            stop,
            thread: Some(thread),
            config: coordinator::Config {
                mailer_state: cfg.mailer_state,
                sender_credential: sender,
                supervisor_binary: cfg.supervisor_binary,
            },
        }
    }
    fn count(&self, op: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["op"] == op)
            .count()
    }
}
impl Drop for Mailer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn coordinator_reservation_is_owner_only_replayed_and_rejects_unresolved_replacement() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let req = request(
        &f.owner,
        Mutation::StartCoordinator {
            run: f.run.clone(),
            expected_revision: 2,
            profile: "auto".into(),
            config: cfg,
        },
    );
    let denied = request(&f.supervisor, req.command.clone());
    assert!(f.store.apply(&denied).is_err());
    let reserved = f.store.apply(&req).unwrap();
    assert_eq!(f.store.apply(&req).unwrap(), reserved);
    assert_eq!(reserved["model_ready"], false);
    assert!(!reserved.to_string().contains("credential"));
    let mut replacement = req;
    replacement.operation_id = id();
    if let Mutation::StartCoordinator {
        expected_revision, ..
    } = &mut replacement.command
    {
        *expected_revision = 3;
    }
    assert!(
        f.store
            .apply(&replacement)
            .unwrap_err()
            .to_string()
            .contains("unresolved")
    );
    assert!(f.store.snapshot(&f.supervisor, &f.run).is_err());
    let ctx = coordinator::context(&f.store, &f.owner, &f.run).unwrap();
    assert_eq!(ctx["coordinator_launches"].as_array().unwrap().len(), 1);
    let p = plan(&f, reserved["id"].as_str().unwrap());
    assert!(!ctx.to_string().contains(&p.credential));
}

#[test]
fn coordinator_lost_create_and_launch_replies_never_duplicate_effects() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let id = start(&mut f, cfg);
    let world = World::new();
    let p = plan(&f, &id);
    let home = launch::files(&f.store.dir, &p).join("home");
    shell(&world, &home, true);
    assert_eq!(
        launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
        "launching"
    );
    for _ in 0..3 {
        assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
    }
    assert_eq!(world.runner.count("workspace create"), 1);
    assert_eq!(world.runner.count("pane run"), 1);
    // A late foreign agent cannot make an unclaimed wrapper authoritative.
    observed(&world, &p, &super::id(), "t1", &home);
    assert!(
        launch::advance(&mut f.store, &id, &world.runner)
            .unwrap_err()
            .to_string()
            .contains("identity")
    );
    assert!(
        f.store
            .connection
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM coordinator_routes)",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
}

#[test]
fn coordinator_lost_registration_stays_uncertain_without_rotating_mailer_session() {
    let mut f = Fixture::new();
    runtime(&mut f, "pi");
    let mailer = Mailer::new(&f, true);
    let cfg = config(&f);
    let id = start(&mut f, cfg);
    let p = plan(&f, &id);
    let world = World::new();
    let home = launch::files(&f.store.dir, &p).join("home");
    shell(&world, &home, false);
    assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
    assert!(
        launch::advance(&mut f.store, &id, &world.runner)
            .unwrap_err()
            .to_string()
            .contains("uncertain")
    );
    assert_eq!(mailer.count("register"), 1);
    assert_eq!(world.runner.count("pane run"), 0);
    assert_eq!(world.runner.count("workspace create"), 1);
}

#[test]
fn coordinator_wrapper_child() {
    let Ok(state) = std::env::var("HS_COORDINATOR_TEST_STATE") else {
        return;
    };
    launch::run_owned(
        Path::new(&state),
        &std::env::var("HS_COORDINATOR_TEST_LAUNCH").unwrap(),
    )
    .unwrap();
}
fn child(f: &Fixture, id: &str) -> WrapperChild {
    let mut c = std::process::Command::new(std::env::current_exe().unwrap());
    c.args([
        "--exact",
        "control::tests::coordinator_launch_tests::coordinator_wrapper_child",
        "--nocapture",
    ])
    .env("HS_COORDINATOR_TEST_STATE", &f.store.dir)
    .env("HS_COORDINATOR_TEST_LAUNCH", id)
    .env("HERDR_SUPERVISOR_ATTEMPT", "parent-attempt")
    .env("HERDR_SUPERVISOR_EPOCH", "99")
    .env("HERDR_SUPERVISOR_CREDENTIAL", "parent-credential")
    .env("HERDR_MAILER_CONFIG", "parent-config")
    .stdout(std::process::Stdio::null());
    WrapperChild(c.spawn().unwrap())
}
fn wait_running(f: &Fixture, id: &str) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let running:bool=f.store.connection.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_processes WHERE launch=?1 AND state='running')",[id],|r|r.get(0)).unwrap();
        if running && f.dir.path().join("started").exists() {
            return;
        }
        assert!(Instant::now() < end, "coordinator did not start");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn stub(f: &Fixture) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let file = f.dir.path().join("native-stub");
    // Keep the actual child alive while the controller binds and fences it.
    std::fs::write(&file,format!("#!/bin/sh\nset -eu\ntest -z \"${{HERDR_SUPERVISOR_ATTEMPT+x}}\"\ntest -z \"${{HERDR_SUPERVISOR_EPOCH+x}}\"\ntest \"$HERDR_SUPERVISOR_CREDENTIAL\" != parent-credential\ntest \"${{HERDR_MAILER_CONFIG-}}\" != parent-config\nprintf '%s\\n' \"$HERDR_SUPERVISOR_GENERATION\" > {}\nexec sleep 60\n",crate::remote::quote(f.dir.path().join("started").to_str().unwrap()))).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    file
}

#[test]
fn three_coordinator_runtimes_bind_exact_native_sessions_bootstrap_and_stop_on_retirement() {
    coordinator_native(false);
}

#[test]
fn codex_registration_recovery_checks_live_owner_native_identity_and_fresh_snapshot() {
    coordinator_native(true);
}

fn coordinator_native(recover_registration: bool) {
    for name in ["codex", "claude", "pi"] {
        if recover_registration && name != "codex" {
            continue;
        }
        let mut f = Fixture::new();
        runtime(&mut f, name);
        let mailer = Mailer::new(&f, recover_registration);
        let mut cfg = config(&f);
        cfg.claude_development_channel = true;
        cfg.codex_binary = stub(&f);
        cfg.mailer_runtime_binary = cfg.codex_binary.clone();
        let id = start(&mut f, cfg);
        let p = plan(&f, &id);
        let home = launch::files(&f.store.dir, &p).join("home");
        let world = World::new();
        let process_observation = std::rc::Rc::new(std::cell::RefCell::new(r#"{"result":{"process_info":{"shell_pid":100,"foreground_process_group_id":100,"foreground_processes":[{"pid":100}]}}}"#.to_string()));
        let observation_change = std::rc::Rc::new(std::cell::Cell::new(0u8));
        if recover_registration {
            let reply = process_observation.clone();
            let change = observation_change.clone();
            let db = f.store.dir.join("control.sqlite");
            let target = id.clone();
            world.runner.on_fn(
                |c| c.display().contains("pane process-info"),
                move |_| {
                    if change.get() != 0 {
                        let c = rusqlite::Connection::open(&db).unwrap();
                        c.busy_timeout(Duration::from_millis(200)).unwrap();
                        if change.get() == 1 {
                            c.execute(
                                "UPDATE coordinator_processes SET state='exited' WHERE launch=?1",
                                [&target],
                            )
                            .unwrap();
                        } else {
                            c.execute(
                                "INSERT OR REPLACE INTO meta VALUES ('unrelated-test-write','1')",
                                [],
                            )
                            .unwrap();
                        }
                    }
                    Ok(ok(&reply.borrow()))
                },
            );
        }
        shell(&world, &home, true);
        launch::advance(&mut f.store, &id, &world.runner).unwrap();
        let inv = launch::invocation(&f.store.dir, &p).unwrap();
        assert_eq!(inv.cwd, home);
        assert!(
            !inv.argv
                .iter()
                .any(|s| s.contains("danger") || s.contains("bypass"))
        );
        if name == "codex" {
            assert_eq!(&inv.argv[..2], ["--approve-for-me", "--cd"]);
            assert!(!inv.argv.iter().any(|arg| arg == "--sandbox"));
            assert_eq!(inv.argv[inv.argv.len() - 2], "--");
            assert!(
                inv.argv
                    .last()
                    .unwrap()
                    .contains("before that bootstrap message arrives")
            );
        } else {
            assert!(inv.environment["HERDR_MAILER_CONFIG"].ends_with("mailer.json"));
        }
        let mut child = child(&f, &id);
        wait_running(&f, &id);
        assert!(
            launch::run_owned(&f.store.dir, &id)
                .unwrap_err()
                .to_string()
                .contains("claimed")
        );
        let p = plan(&f, &id);
        let uuid = p.expected_session.clone().unwrap_or_else(super::id);
        observed(&world, &p, &uuid, "changed-terminal", &home);
        assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
        observed(&world, &p, &uuid, "t1", &home);
        if recover_registration {
            assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
            assert_eq!(mailer.count("register"), 1);
            let process_doc: String = f
                .store
                .connection
                .query_row(
                    "SELECT doc FROM coordinator_processes WHERE launch=?1",
                    [&id],
                    |r| r.get(0),
                )
                .unwrap();
            let process: Value = serde_json::from_str(&process_doc).unwrap();
            let pgid = process["child_pid"].as_u64().unwrap();
            crate::control::process::require_live_owner(&f.store.dir, &process).unwrap();
            let process_info = json!({"result":{"process_info":{"foreground_process_group_id":pgid,"foreground_processes":[{"pid":pgid}]}}}).to_string();
            *process_observation.borrow_mut() = process_info.clone();
            let recover = || {
                crate::control::registration_recovery::recover_observed(
                    &f.store.dir,
                    false,
                    &id,
                    p.generation,
                    "recover-codex",
                    &world.runner,
                )
            };
            observed(&world, &p, &super::id(), "t1", &home);
            assert!(recover().is_err(), "changed native UUID");
            observed(&world, &p, &uuid, "foreign-terminal", &home);
            assert!(recover().is_err(), "changed terminal");
            observed(&world, &p, &uuid, "t1", &home);
            *process_observation.borrow_mut() = r#"{"result":{"process_info":{"foreground_process_group_id":1,"foreground_processes":[{"pid":1}]}}}"#.into();
            assert!(recover().is_err(), "foreground PID must be the owned child");
            *process_observation.borrow_mut() = process_info.clone();
            let mut legacy = process.clone();
            legacy.as_object_mut().unwrap().remove("owner_nonce");
            f.store
                .connection
                .execute(
                    "UPDATE coordinator_processes SET doc=?2 WHERE launch=?1",
                    rusqlite::params![id, legacy.to_string()],
                )
                .unwrap();
            assert!(recover().is_err(), "legacy process receipt is insufficient");
            f.store
                .connection
                .execute(
                    "UPDATE coordinator_processes SET doc=?2 WHERE launch=?1",
                    rusqlite::params![id, process_doc],
                )
                .unwrap();
            // A process writer can persist exit during read-only Herdr inspection.
            // Revalidation must reject changed process evidence without resetting it.
            observation_change.set(1);
            assert!(recover().is_err(), "changed observation snapshot");
            assert!(plan(&f, &id).registration_started);
            f.store
                .connection
                .execute(
                    "UPDATE coordinator_processes SET state='running' WHERE launch=?1",
                    [&id],
                )
                .unwrap();
            *process_observation.borrow_mut() = process_info.clone();
            observation_change.set(2); // An unrelated write must not starve this recovery.
            let receipt = recover().unwrap();
            assert_eq!(receipt["stage"], "codex_before_binding");
            assert_eq!(recover().unwrap(), receipt);
            mailer.lose_register.store(false, Ordering::Relaxed);
        }
        assert_eq!(
            launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
            "bound"
        );
        launch::advance(&mut f.store, &id, &world.runner).unwrap();
        assert!(
            crate::control::registration_recovery::recover_observed(
                &f.store.dir,
                false,
                &id,
                p.generation,
                "too-late",
                &world.runner
            )
            .is_err()
        );
        assert_eq!(
            mailer.count("register"),
            if recover_registration { 2 } else { 1 }
        );
        let ctx = coordinator::context(&f.store, &p.credential, &f.run).unwrap();
        assert_eq!(
            ctx["native_coordinator"]["endpoint"]["native_session"],
            uuid
        );
        coordinator::deliver(&mut f.store, &mailer.config).unwrap();
        assert_eq!(mailer.count("send"), 1);
        let sent = mailer
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|r| r["op"] == "send")
            .unwrap()
            .clone();
        assert_eq!(
            sent["recipientSession"],
            plan(&f, &id).registration.unwrap()["id"]
        );
        assert_eq!(
            sent["recipientEpoch"],
            if recover_registration { 2 } else { 1 }
        );
        let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        call(
            &mut f.store,
            &f.owner,
            Mutation::BindSupervisor {
                run: f.run.clone(),
                expected_revision: r.revision,
            },
        );
        assert!(child.wait().success());
        assert_eq!(
            launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
            "exited"
        );
        assert!(f.store.snapshot(&p.credential, &f.run).is_err());
        let receipt: String = f
            .store
            .connection
            .query_row(
                "SELECT doc FROM coordinator_processes WHERE launch=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&receipt).unwrap()["reason"],
            "cancelled"
        );
        *world.agents.borrow_mut() = "[]".into();
        *process_observation.borrow_mut() = r#"{"result":{"process_info":{"shell_pid":100,"foreground_process_group_id":100,"foreground_processes":[{"pid":100}]}}}"#.into();
        retirement_tests::retire_exited(&mut f, &world, &id);
        let cfg = config(&f);
        let next = start(
            &mut f,
            launch::Config {
                claude_development_channel: true,
                ..cfg
            },
        );
        assert_ne!(next, id);
        assert_eq!(world.runner.count("workspace create"), 1);
        assert_eq!(world.runner.count("pane run"), 1);
    }
}

#[test]
fn fenced_unclaimed_coordinator_cannot_start_late() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let id = start(&mut f, cfg);
    let p = plan(&f, &id);
    let world = World::new();
    shell(&world, &launch::files(&f.store.dir, &p).join("home"), false);
    launch::advance(&mut f.store, &id, &world.runner).unwrap();
    let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.owner,
        Mutation::Cancel {
            run: f.run.clone(),
            expected_revision: r.revision,
        },
    );
    assert_eq!(
        launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
        "exited"
    );
    assert!(launch::run_owned(&f.store.dir, &id).is_err());
    assert!(
        f.store
            .connection
            .query_row(
                "SELECT NOT EXISTS(SELECT 1 FROM coordinator_processes)",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
}

#[test]
fn schema_four_migration_preserves_active_coordinator_routes_and_memory() {
    let mut f = Fixture::new();
    let bound = call(
        &mut f.store,
        &f.owner,
        Mutation::BindCoordinator {
            run: f.run.clone(),
            expected_revision: 2,
            endpoint: coordinator::Endpoint {
                agent: "existing".into(),
                mailer_session: id(),
                mailer_epoch: 1,
                runtime: "codex".into(),
                native_session: id(),
            },
        },
    );
    let cap = text(&bound, "credential");
    let proposal = call(
        &mut f.store,
        &cap,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "testing".into(),
            text: "Run the corpus".into(),
            source_attempt: None,
            evidence: vec!["corpus.md".into()],
        },
    );
    call(
        &mut f.store,
        &cap,
        Mutation::ApproveMemory {
            proposal: text(&proposal, "id"),
            expected_revision: 0,
        },
    );
    let before = coordinator::context(&f.store, &cap, &f.run).unwrap();
    f.store.connection.execute_batch("DROP TABLE coordinator_processes; DROP TABLE coordinator_launches; DROP TABLE integration_policies; DROP TABLE attempt_profiles; DROP TABLE artifact_chunks; DROP TABLE artifact_uploads; DROP TABLE reception_failures; DROP TABLE event_recoveries; DROP TABLE registration_recoveries; DROP TABLE task_questions; DROP TABLE artifact_retirements; DROP TABLE coordinator_retirements; DROP TABLE verification_retirements; ALTER TABLE nodes DROP COLUMN draining; ALTER TABLE nodes DROP COLUMN revision; PRAGMA user_version=4;").unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    let after = coordinator::context(&f.store, &cap, &f.run).unwrap();
    assert_eq!(after["native_coordinator"], before["native_coordinator"]);
    assert_eq!(after["notices"], before["notices"]);
    assert_eq!(after["memory"], before["memory"]);
    assert_eq!(after["coordinator_launches"], json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controller_service_pumps_coordinator_intents_and_reconciles_after_restart() {
    use crate::control::{git_pipeline, service, transport::Broker};
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new();
    let mut cfg = config(&f);
    let script = f.dir.path().join("herdr-stub");
    let marker = f.dir.path().join("creates");
    std::fs::write(&script,format!("#!/bin/sh\ncase \"$*\" in\n*'pane list'*) printf '%s\\n' '{{\"result\":{{\"panes\":[]}}}}';;\n*'workspace create'*) printf 'create\\n' >> {}; exit 1;;\n*) exit 2;;\nesac\n",crate::remote::quote(marker.to_str().unwrap()))).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    cfg.herdr_binary = script;
    let notification = coordinator::Config {
        mailer_state: cfg.mailer_state.clone(),
        sender_credential: f.dir.path().join("unused-sender"),
        supervisor_binary: cfg.supervisor_binary.clone(),
    };
    let launch = start(&mut f, cfg);
    let owner = f.store.owner().unwrap();
    let server = NatsServer::start(&owner);
    let config = server.config("admin");
    let broker = Broker::connect(
        &config,
        &owner,
        crate::control::broker_policy::Endpoint::Provisioner,
    )
    .await
    .unwrap();
    broker
        .provision(&["controller".into(), "local".into()])
        .await
        .unwrap();
    let path = f.dir.path().join("broker.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    for _ in 0..2 {
        service::controller(
            f.store.dir.clone(),
            path.clone(),
            git_pipeline::Config {
                repositories: BTreeMap::new(),
                check_timeout_secs: 5,
            },
            true,
            Some(notification.clone()),
        )
        .await
        .unwrap();
        assert!(!f.store.dir.join("control.sock").exists());
    }
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "create\n");
    let summary = launch::summaries(&f.store.connection, &f.run).unwrap();
    assert_eq!(summary[0]["state"], "preparing");
    assert!(
        summary[0]["last_error"]
            .as_str()
            .unwrap()
            .contains("unresolved")
    );
    assert!(plan(&f, &launch).workspace_started);
    assert!(!plan(&f, &launch).launch_started);
}

#[test]
fn coordinator_owner_decision_is_retryable_without_blocking_during_launch_effect() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let request = request(
        &f.owner,
        Mutation::StartCoordinator {
            run: f.run.clone(),
            expected_revision: 2,
            profile: "auto".into(),
            config: cfg,
        },
    );
    let held = crate::control::api::lock(&f.store.dir, "coordinator-launch.lock").unwrap();
    let started = Instant::now();
    assert!(
        f.store
            .apply(&request)
            .unwrap_err()
            .to_string()
            .contains("another process")
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        document::<Run>(&f.store.connection, "runs", &f.run)
            .unwrap()
            .revision,
        2
    );
    drop(held);
    let result = f.store.apply(&request).unwrap();
    assert_eq!(f.store.apply(&request).unwrap(), result);
}

#[test]
fn relative_controller_state_keeps_absolute_coordinator_home_and_capability_paths() {
    let dir = tempfile::tempdir_in(".").unwrap();
    let relative = Path::new(".")
        .join(dir.path().file_name().unwrap())
        .join("state");
    assert!(!relative.is_absolute());
    let store = Store::initialize(&relative).unwrap();
    assert_eq!(store.dir, std::fs::canonicalize(&relative).unwrap());
    assert_eq!(Store::open(&relative).unwrap().dir, store.dir);
    assert!(coordinator::credential_path(&store.dir, "run", 1).is_absolute());
}

#[test]
fn run_profiles_changes_preserve_coordinator_launch_snapshot() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let launch = start(&mut f, cfg);
    let before = plan(&f, &launch);
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.owner,
        Mutation::SetProfile {
            run: f.run.clone(),
            expected_revision: run.revision,
            name: "auto".into(),
            profile: Some(Profile {
                runtime: "pi".into(),
                argv: vec!["--model".into(), "future".into()],
                limit: 1,
            }),
        },
    );
    let after = plan(&f, &launch);
    assert_eq!(before.profile, after.profile);
    assert_eq!(after.profile.runtime, "codex");
    assert_eq!(before.expected_session, after.expected_session);
    assert_eq!(before.credential, after.credential);
    assert!(!after.launch_started);
}

#[test]
fn registration_recovery_coordinator_retries_only_after_explicit_prelaunch_authorization() {
    let mut f = Fixture::new();
    runtime(&mut f, "pi");
    let mailer = Mailer::new(&f, true);
    let cfg = config(&f);
    let id = start(&mut f, cfg);
    let p = plan(&f, &id);
    let world = World::new();
    let home = launch::files(&f.store.dir, &p).join("home");
    shell(&world, &home, false);
    assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
    let before = plan(&f, &id);
    let recover = || {
        crate::control::registration_recovery::recover(
            &f.store.dir,
            false,
            &id,
            p.generation,
            "recover-first",
        )
    };
    let receipt = recover().unwrap();
    assert_eq!(recover().unwrap(), receipt);
    assert_eq!(
        mailer.count("register"),
        1,
        "authorization performs no registration"
    );
    let after = plan(&f, &id);
    assert!(!after.registration_started);
    assert_eq!(after.expected_session, before.expected_session);
    assert_eq!(after.pane, before.pane);
    assert_eq!(after.credential, before.credential);
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM registration_recoveries", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    mailer.lose_register.store(false, Ordering::Relaxed);
    assert_eq!(
        launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
        "launching"
    );
    assert_eq!(mailer.count("register"), 2);
    assert_eq!(world.runner.count("pane run"), 1);
    assert_eq!(world.runner.count("workspace create"), 1);
    assert_eq!(
        crate::control::registration_recovery::recover(
            &f.store.dir,
            false,
            &id,
            p.generation,
            "recover-first"
        )
        .unwrap(),
        receipt
    );
    assert!(
        plan(&f, &id).registration_started,
        "replaying authorization cannot reset a launched receiver"
    );
    assert!(
        crate::control::registration_recovery::recover(
            &f.store.dir,
            false,
            &id,
            p.generation,
            "recover-again"
        )
        .is_err()
    );
    let journal: u64 = f
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM journal WHERE kind='registration_recovery_authorized'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(journal, 1);
    assert!(!receipt.to_string().contains("token"));
}

#[test]
fn registration_recovery_coordinator_fences_files_processes_generation_and_rolls_back_journal_failure()
 {
    let mut f = Fixture::new();
    runtime(&mut f, "claude");
    let mut cfg = config(&f);
    cfg.claude_development_channel = true;
    let id = start(&mut f, cfg);
    let mut p = plan(&f, &id);
    p.registration_started = true;
    let original = serde_json::to_string(&p).unwrap();
    f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET state='launching',doc=?2 WHERE id=?1",
            rusqlite::params![id, original],
        )
        .unwrap();
    let state = f.store.dir.clone();
    let recover = |epoch| {
        crate::control::registration_recovery::recover(&state, false, &id, epoch, "recover")
    };
    assert!(recover(p.generation + 1).is_err());
    let lock = crate::control::api::lock(&state, "coordinator-launch.lock").unwrap();
    assert!(recover(p.generation).is_err());
    drop(lock);
    let dir = launch::files(&state, &p);
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "registration.json",
        "mailer.token",
        "mailer.json",
        "native-receipts",
    ] {
        let file = dir.join(name);
        std::os::unix::fs::symlink("missing-evidence", &file).unwrap();
        assert!(
            recover(p.generation)
                .unwrap_err()
                .to_string()
                .contains("receiver evidence")
        );
        std::fs::remove_file(file).unwrap();
    }
    f.store
        .connection
        .execute(
            "INSERT INTO coordinator_processes VALUES (?1,?2,'spawn_started','{}')",
            rusqlite::params![id, p.generation],
        )
        .unwrap();
    assert!(recover(p.generation).is_err());
    f.store
        .connection
        .execute("DELETE FROM coordinator_processes WHERE launch=?1", [&id])
        .unwrap();
    for field in ["launch_started", "wrapper_started"] {
        let mut changed = serde_json::to_value(&p).unwrap();
        changed[field] = json!(true);
        f.store
            .connection
            .execute(
                "UPDATE coordinator_launches SET doc=?2 WHERE id=?1",
                rusqlite::params![id, changed.to_string()],
            )
            .unwrap();
        assert!(recover(p.generation).is_err());
    }
    f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET doc=?2 WHERE id=?1",
            rusqlite::params![id, original],
        )
        .unwrap();
    f.store.connection.execute_batch("CREATE TRIGGER reject_recovery_journal BEFORE INSERT ON journal WHEN NEW.kind='registration_recovery_authorized' BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;").unwrap();
    assert!(recover(p.generation).is_err());
    assert!(plan(&f, &id).registration_started);
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM registration_recoveries", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER reject_recovery_journal")
        .unwrap();
    f.store
        .connection
        .execute(
            "UPDATE principals SET active=0 WHERE run=?1 AND role='supervisor'",
            [&p.run],
        )
        .unwrap();
    assert!(recover(p.generation).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn coordinator_recovers_crashed_wrapper_only_after_native_exit_and_retries_retirement() {
    let mut f = Fixture::new();
    runtime(&mut f, "codex");
    let mailer = Mailer::new(&f, false);
    let mut cfg = config(&f);
    cfg.codex_binary = stub(&f);
    let marker = f.dir.path().join("started");
    let release = f.dir.path().join("release");
    struct Release(std::path::PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"release");
        }
    }
    let cleanup = Release(release.clone());
    std::fs::write(&cfg.codex_binary, format!("#!/bin/sh\n: > {}\nn=0\nwhile [ ! -f {} ] && [ $n -lt 1000 ]; do n=$((n+1)); sleep 0.02; done\nexit 7\n", crate::remote::quote(marker.to_str().unwrap()), crate::remote::quote(release.to_str().unwrap()))).unwrap();
    let id = start(&mut f, cfg);
    let p = plan(&f, &id);
    let home = launch::files(&f.store.dir, &p).join("home");
    let world = World::new();
    shell(&world, &home, false);
    launch::advance(&mut f.store, &id, &world.runner).unwrap();
    let mut wrapper = child(&f, &id);
    wait_running(&f, &id);
    let p = plan(&f, &id);
    let uuid = p.expected_session.clone().unwrap_or_else(super::id);
    observed(&world, &p, &uuid, "t1", &home);
    assert_eq!(
        launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
        "bound"
    );
    let read_process = |f: &Fixture| -> Value {
        let doc: String = f
            .store
            .connection
            .query_row(
                "SELECT doc FROM coordinator_processes WHERE launch=?1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        serde_json::from_str(&doc).unwrap()
    };
    let original = read_process(&f);
    wrapper.0.kill().unwrap();
    assert!(!wrapper.wait().success());
    assert_eq!(
        launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
        "bound"
    );
    assert_eq!(
        read_process(&f),
        original,
        "live orphan must not be declared exited"
    );
    assert!(f.store.snapshot(&p.credential, &f.run).is_ok());
    drop(cleanup);
    let until = Instant::now() + Duration::from_secs(5);
    while crate::control::kernel_process::exited(&original["kernel_identity"])
        .unwrap()
        .is_none()
    {
        assert!(Instant::now() < until, "native fixture did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    f.store.connection.execute_batch("CREATE TRIGGER fail_retire BEFORE UPDATE ON principals WHEN NEW.active=0 BEGIN SELECT RAISE(ABORT,'injected retirement loss'); END;").unwrap();
    assert!(launch::advance(&mut f.store, &id, &world.runner).is_err());
    let receipt = read_process(&f);
    assert_eq!(receipt["reason"], "recovered_native_exit");
    assert!(
        receipt["code"].is_null(),
        "fixture exits 7, but a non-parent cannot recover its status"
    );
    assert!(
        f.store.snapshot(&p.credential, &f.run).is_ok(),
        "failed retirement must roll back"
    );
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_retire")
        .unwrap();
    f.store = Store::open(&f.store.dir).unwrap();
    for _ in 0..2 {
        assert_eq!(
            launch::advance(&mut f.store, &id, &world.runner).unwrap()["state"],
            "exited"
        );
    }
    assert!(f.store.snapshot(&p.credential, &f.run).is_err());
    let routes: u64 = f
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM coordinator_routes WHERE active=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(routes, 0);
    let exits: u64 = f
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM journal WHERE kind='coordinator_exited'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(exits, 1);
    assert_eq!(world.runner.count("workspace create"), 1);
    assert_eq!(world.runner.count("pane run"), 1);
    assert_eq!(mailer.count("register"), 1);
}
