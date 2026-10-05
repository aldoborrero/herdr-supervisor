//! End-to-end checks of the built binary with a scrubbed environment.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_herdr-supervisor");

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
