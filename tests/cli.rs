//! End-to-end checks of the built binary with a scrubbed environment.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_herdr-supervisor");

#[test]
fn managed_create_publishes_one_bound_view_through_real_controller_ipc_without_launching_agents() {
    use serde_json::{Value, json};
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    struct Api(std::process::Child);
    impl Drop for Api {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("controller");
    let root = home.path().join("project views");
    let state_arg = state.to_str().unwrap();
    let root_arg = root.to_str().unwrap();
    assert!(
        hp(home.path(), &["control", "--state", state_arg, "init"])
            .status
            .success()
    );
    let mut api = Api(Command::new(BIN)
        .env_clear()
        .env("HOME", home.path())
        .args(["control", "--state", state_arg, "serve"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap());
    let until = Instant::now() + Duration::from_secs(5);
    while !state.join("control.sock").exists() {
        assert!(api.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let spec = json!({"name":"Managed demo","controller":state,"credential":state.join("admin.token"),
        "repo":"/logical/repository","base_oid":"1111111111111111111111111111111111111111",
        "objective":"Keep literal $HOME and `text`",
        "policy":{"max_active":2,"reserved_reviewers":1,"max_attempts":3,"duration_secs":3600,
            "require_review":true,"profiles":{"auto":{"runtime":"codex","argv":[],"limit":2}},"checks":{}}});
    let file = home.path().join("setup.json");
    std::fs::write(&file, serde_json::to_vec(&spec).unwrap()).unwrap();
    let first = hp(
        home.path(),
        &[
            "--root",
            root_arg,
            "managed",
            "create",
            "--spec",
            file.to_str().unwrap(),
        ],
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let replay = hp(
        home.path(),
        &[
            "--root",
            root_arg,
            "managed",
            "create",
            "--spec-json",
            &spec.to_string(),
        ],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(first.stdout, replay.stdout);
    let resumed = hp(
        home.path(),
        &[
            "--root",
            root_arg,
            "managed",
            "resume-setup",
            "managed-demo",
        ],
    );
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(resumed.stdout, first.stdout);
    let local = hp(
        home.path(),
        &[
            "--root",
            root_arg,
            "managed",
            "setup-status",
            "managed-demo",
        ],
    );
    assert!(local.status.success());
    let local: Value = serde_json::from_slice(&local.stdout).unwrap();
    assert_eq!(local["recorded_state"], "published");
    assert_eq!(local["current_controller_state_verified"], false);
    let receipt: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(receipt["models_started"], false);
    let snapshot = hp(
        home.path(),
        &["--root", root_arg, "managed", "status", "managed-demo"],
    );
    assert!(snapshot.status.success());
    let snapshot: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    assert_eq!(snapshot["run"]["id"], receipt["run"]);
    assert_eq!(snapshot["run"]["objective"], spec["objective"]);
    assert_eq!(snapshot["active_attempts"], 0);
    assert_eq!(snapshot["coordinator_launches"], json!([]));
    let token = std::fs::read_to_string(state.join("admin.token")).unwrap();
    assert!(!String::from_utf8_lossy(&first.stdout).contains(&token));
    let legacy = hp(
        home.path(),
        &[
            "--root",
            root_arg,
            "thread",
            "stop",
            "managed-demo",
            "t-0001",
        ],
    );
    assert!(!legacy.status.success());
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("managed by the controller"));
    // The task form sends larger contracts over stdin, not one OS argument.
    let instructions = "\\".repeat(65536);
    let request = serde_json::to_vec(&json!({"method":"create_task", "params":{
        "run":receipt["run"], "expected_revision":snapshot["run"]["revision"],
        "contract":{"title":"Large literal contract", "instructions":instructions,
            "criteria":["Preserve text"],"kind":"code","checks":[],"memory_topics":[]},
        "dependencies":[]}}))
    .unwrap();
    assert!(request.len() > 131072);
    let submit = || {
        use std::io::Write;
        let mut child = Command::new(BIN)
            .env_clear()
            .env("HOME", home.path())
            .args([
                "--root",
                root_arg,
                "managed",
                "apply",
                "managed-demo",
                "--operation",
                "large-task-form",
                "--request",
                "-",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&request).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let task = submit();
    assert_eq!(task["contract"]["instructions"], instructions);
    assert_eq!(submit(), task);
    let latest = hp(
        home.path(),
        &["--root", root_arg, "managed", "status", "managed-demo"],
    );
    assert!(latest.status.success());
    let latest: Value = serde_json::from_slice(&latest.stdout).unwrap();
    assert_eq!(latest["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(latest["active_attempts"], 0);
}

#[test]
fn continuous_services_deliver_review_integrate_and_stop_across_node_restart() {
    continuous_workflow(false);
}

#[test]
fn continuous_services_verify_artifacts_before_native_review_and_complete_without_git_integration()
{
    continuous_workflow(true);
}

fn continuous_workflow(artifact: bool) {
    let python = std::env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let nats = std::env::var_os("NATS_SERVER").unwrap_or_else(|| "nats-server".into());
    let mut command = Command::new(python);
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/service_workflow.py"
        ))
        .args(["--binary", BIN, "--nats-server"])
        .arg(nats);
    if artifact {
        command.arg("--artifact");
    }
    let output = command
        .output()
        .expect("Python 3 and NATS are required for the continuous service acceptance test");
    assert!(
        output.status.success(),
        "service workflow failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn managed_control_help_and_bootstrap_are_usable_without_herdr() {
    let home = tempfile::tempdir().unwrap();
    assert!(hp(home.path(), &["control", "--help"]).status.success());
    let state = home.path().join("controller");
    let args = ["control", "--state", state.to_str().unwrap(), "init"];
    let first = hp(home.path(), &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = hp(home.path(), &args);
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout);
    let token = std::fs::read_to_string(state.join("admin.token")).unwrap();
    assert!(!String::from_utf8(first.stdout).unwrap().contains(&token));
}

fn hp(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .env_clear()
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn connect_native_coordinator_publishes_private_capability_and_context_without_printing_it() {
    use serde_json::{Value, json};
    use std::{
        io::{BufRead, Write},
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let home = tempfile::tempdir().unwrap();
    let state = home.path().join("controller");
    let state_arg = state.to_str().unwrap();
    assert!(
        hp(home.path(), &["control", "--state", state_arg, "init"])
            .status
            .success()
    );
    let admin = state.join("admin.token");
    let input = home.path().join("create.json");
    std::fs::write(&input,serde_json::to_vec(&json!({"method":"create_run","params":{"project":"demo","repo":"/fixture/repo","base_oid":"1111111111111111111111111111111111111111","objective":"test coordinator attachment","policy":{"max_active":2,"reserved_reviewers":1,"max_attempts":2,"duration_secs":3600,"require_review":true,"profiles":{"auto":{"runtime":"codex","argv":[],"limit":2}},"checks":{}}}})).unwrap()).unwrap();
    let created = hp(
        home.path(),
        &[
            "control",
            "--state",
            state_arg,
            "apply",
            "--credential",
            admin.to_str().unwrap(),
            "--operation",
            "create",
            "--request",
            input.to_str().unwrap(),
        ],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let run: Value = serde_json::from_slice(&created.stdout).unwrap();
    let run = run["id"].as_str().unwrap();
    let mut api = Server(
        Command::new(BIN)
            .env_clear()
            .env("HOME", home.path())
            .args(["control", "--state", state_arg, "serve"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(5);
    while !state.join("control.sock").exists() {
        assert!(api.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mailer = home.path().join("mailer");
    std::fs::create_dir(&mailer).unwrap();
    let receiver = mailer.join("receiver.token");
    std::fs::write(&receiver, "receiver-fixture-0000000000000000000000000000").unwrap();
    std::fs::set_permissions(&receiver, std::fs::Permissions::from_mode(0o600)).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(mailer.join("daemon.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let doctor = std::thread::spawn(move || {
        for _ in 0..2 {
            let until = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until);
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["op"],
                "doctor"
            );
            writeln!(stream,"{}",json!({"data":{"agent":"coordinator","session":"11111111-1111-4111-8111-111111111111","epoch":1,"runtime":"codex","location":"22222222-2222-4222-8222-222222222222"}})).unwrap();
        }
    });
    let arguments = [
        "control",
        "--state",
        state_arg,
        "connect-supervisor",
        "--credential",
        admin.to_str().unwrap(),
        "--runtime-credential",
        receiver.to_str().unwrap(),
        "--mailer-state",
        mailer.to_str().unwrap(),
        "--operation",
        "bind-native",
        "--expected-revision",
        "1",
        run,
    ];
    let first = hp(home.path(), &arguments);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let second = hp(home.path(), &arguments);
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout);
    doctor.join().unwrap();
    let bound: Value = serde_json::from_slice(&first.stdout).unwrap();
    let credential = bound["credential_file"].as_str().unwrap();
    let token = std::fs::read_to_string(credential).unwrap();
    assert!(!String::from_utf8_lossy(&first.stdout).contains(&token));
    assert_eq!(
        std::fs::metadata(credential).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let context = hp(
        home.path(),
        &[
            "control",
            "--state",
            state_arg,
            "context",
            "--credential",
            credential,
            run,
        ],
    );
    assert!(context.status.success());
    let context: Value = serde_json::from_slice(&context.stdout).unwrap();
    assert_eq!(context["run"]["id"], run);
    assert!(
        context["instructions"]
            .as_str()
            .unwrap()
            .contains("independent reviewer")
    );
    assert!(!context.to_string().contains(&token));
    let root = home.path().join("projects root");
    let root_arg = root.to_str().unwrap();
    assert!(
        hp(home.path(), &["--root", root_arg, "new", "demo"])
            .status
            .success()
    );
    let attach_args = [
        "--root",
        root_arg,
        "managed",
        "attach",
        "demo",
        "--controller",
        state_arg,
        "--credential",
        admin.to_str().unwrap(),
        "--run",
        run,
    ];
    for _ in 0..2 {
        let attached = hp(home.path(), &attach_args);
        assert!(
            attached.status.success(),
            "{}",
            String::from_utf8_lossy(&attached.stderr)
        );
    }
    let ctx = hp(home.path(), &["--root", root_arg, "context", "demo"]);
    assert!(ctx.status.success());
    let ctx: Value = serde_json::from_slice(&ctx.stdout).unwrap();
    assert_eq!(ctx["run"]["id"], run);
    let pause =
        json!({"method":"pause","params":{"run":run,"expected_revision":ctx["run"]["revision"]}})
            .to_string();
    let pause_args = [
        "--root",
        root_arg,
        "managed",
        "apply",
        "demo",
        "--operation",
        "panel-pause",
        "--request-json",
        &pause,
    ];
    let paused = hp(home.path(), &pause_args);
    assert!(
        paused.status.success(),
        "{}",
        String::from_utf8_lossy(&paused.stderr)
    );
    let replay = hp(home.path(), &pause_args);
    assert!(replay.status.success());
    assert_eq!(paused.stdout, replay.stdout);
    assert_eq!(
        serde_json::from_slice::<Value>(&paused.stdout).unwrap()["state"],
        "paused"
    );
    for args in [
        vec!["thread", "stop", "demo", "t-0001"],
        vec!["delete", "demo", "--force"],
        vec!["set", "demo", "goal", "forged"],
    ] {
        let mut full = vec!["--root", root_arg];
        full.extend(args);
        let rejected = hp(home.path(), &full);
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("managed by the controller"));
    }
    assert!(
        hp(home.path(), &["--root", root_arg, "list"])
            .status
            .success()
    );
    assert!(
        hp(home.path(), &["--root", root_arg, "overview", "demo"])
            .status
            .success()
    );
    api.0.kill().unwrap();
    api.0.wait().unwrap();
    std::fs::write(
        root.join("demo/TASKS.md"),
        "# Tasks\n\n- [ ] untrusted fallback\n",
    )
    .unwrap();
    let offline = hp(home.path(), &["--root", root_arg, "overview", "demo"]);
    let text = String::from_utf8(offline.stdout).unwrap();
    assert!(text.contains("controller unavailable"));
    assert!(!text.contains("untrusted fallback"));
}

#[test]
fn context_prints_a_usable_prefix_in_a_scrubbed_environment() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("my root");
    let root_arg = root.to_str().unwrap();
    assert!(
        hp(home.path(), &["--root", root_arg, "new", "Demo"])
            .status
            .success()
    );

    let out = hp(
        home.path(),
        &["--root", root_arg, "context", "demo", "--peek"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let prefix = text
        .lines()
        .next()
        .unwrap()
        .strip_prefix("Commands: ")
        .unwrap();
    // Fixed shape `<binary> --root <root>`, with the spaced root shell-quoted.
    assert_eq!(prefix, format!("{BIN} --root '{root_arg}'"));

    // The printed prefix works as typed, from a bare shell.
    let listed = Command::new("/bin/sh")
        .env_clear()
        .env("HOME", home.path())
        .args(["-c", &format!("{prefix} list")])
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "demo\tactive\tno threads\n"
    );
}

#[test]
fn peek_records_nothing_and_context_records_seen_items() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("root");
    let root_arg = root.to_str().unwrap();
    assert!(
        hp(home.path(), &["--root", root_arg, "new", "demo"])
            .status
            .success()
    );
    let item = "+++\nid = \"20260917T000000Z-routine-r-1\"\nkind = \"routine\"\nsubject = \"r\"\ncreated = \"x\"\nsummary = \"s\"\n+++\n";
    std::fs::write(
        root.join("demo/inbox/20260917T000000Z-routine-r-1.md"),
        item,
    )
    .unwrap();
    let seen = root.join("demo/.state/inbox-seen.json");

    assert!(
        hp(
            home.path(),
            &["--root", root_arg, "context", "demo", "--peek"]
        )
        .status
        .success()
    );
    assert!(!seen.exists());
    assert!(
        hp(home.path(), &["--root", root_arg, "context", "demo"])
            .status
            .success()
    );
    assert!(
        std::fs::read_to_string(&seen)
            .unwrap()
            .contains("routine-r-1")
    );
}

#[test]
fn path_like_names_and_slugs_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("root");
    let root_arg = root.to_str().unwrap();
    assert!(
        !hp(home.path(), &["--root", root_arg, "new", "../x"])
            .status
            .success()
    );
    assert!(
        !hp(home.path(), &["--root", root_arg, "open", "../x"])
            .status
            .success()
    );
    assert!(
        !hp(home.path(), &["--root", root_arg, "context", "../x"])
            .status
            .success()
    );
    assert!(
        !hp(home.path(), &["--root", root_arg, "thread", "list", "../x"])
            .status
            .success()
    );
    assert!(
        !hp(
            home.path(),
            &["--root", root_arg, "delete", "../x", "--force"]
        )
        .status
        .success()
    );
    assert!(!root.exists());
    assert!(!home.path().join("x").exists());
}

#[test]
fn ticker_start_without_projects_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert!(hp(home.path(), &["ticker", "start"]).status.success());
    assert!(!home.path().join(".herdr-supervisor").exists());
    assert!(!home.path().join(".config").exists());
}

#[test]
fn plugin_new_pane_opens_managed_form_and_cancel_creates_no_project() {
    let output = Command::new(std::env::var_os("PYTHON").unwrap_or_else(|| "python3".into()))
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/popup_setup.py"))
        .arg(env!("CARGO_BIN_EXE_herdr-supervisor"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
