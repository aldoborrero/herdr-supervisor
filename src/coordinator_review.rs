//! Regression probes retained from the independent Projects adoption study.
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use crate::runner::fake::{FakeRunner, ok, timeout};
use crate::scenarios::World;
use crate::{remote, steps, thread, threads, ticker};

#[test]
fn capacity_reservation_refuses_second_start() {
    let w = World::new();
    let p = w.project("demo", "a.sock");
    crate::settings::set(&w.ctx(), "demo", "max_parallel_threads", "1").unwrap();
    let repo = w.home.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    w.runner.on("rev-parse --show-toplevel", ok("/repo"));
    w.runner.on("remote get-url", ok(""));
    w.runner.on("symbolic-ref", ok("main"));
    w.runner.on(
        "rev-parse --git-path",
        crate::runner::fake::fail(1, "not git"),
    );
    let n = Rc::new(Cell::new(0));
    let n2 = n.clone();
    let root = w.home.path().to_path_buf();
    w.runner.on_fn(|c| c.display().contains("worktree create"), move |_| {
        let i = n2.get() + 1;
        n2.set(i);
        let wt = root.join(format!("wt{i}"));
        std::fs::create_dir(&wt).unwrap();
        Ok(ok(&format!(r#"{{"result":{{"root_pane":{{"workspace_id":"w{i}","tab_id":"w{i}:t1","pane_id":"w{i}:p1","cwd":"{}"}},"worktree":{{"path":"{}"}}}}}}"#, wt.display(), wt.display())))
    });
    let args = || threads::StartArgs {
        title: "Same task".into(),
        repo: Some(repo.to_string_lossy().into_owned()),
        machine: None,
        profile: None,
        kind: None,
        base: Some("main".into()),
        task: "Same request".into(),
    };
    let a = threads::start(&w.ctx(), "demo", args()).unwrap();
    assert!(threads::start(&w.ctx(), "demo", args()).is_err());
    assert_eq!(a.id, "t-0001");
    assert_eq!(thread::list(&p).len(), 1);
    assert_eq!(n.get(), 1);
}

#[test]
fn remote_branch_timeout_is_unknown() {
    let runner = FakeRunner::new();
    runner.on("ssh", timeout());
    assert!(remote::branch_exists(&runner, "worker", "/repo", "hp/demo/t-0001").is_err());
}

#[test]
fn shared_machine_polling_reaches_each_project() {
    let base = World::new();
    let a = base.project("alpha", "a.sock");
    let b = base.project("beta", "a.sock");
    base.thread(&a, Path::new("/remote/alpha"), |t| t.machine = "box".into());
    base.thread(&b, Path::new("/remote/beta"), |t| {
        t.machine = "box".into();
        t.prompt_pending = true;
        t.workspace_id = "w3".into();
        t.tab_id = "w3:t1".into();
        t.pane_id = "w3:p1".into();
    });
    let w = World {
        runner: FakeRunner::new(),
        ..base
    };
    w.runner.on_fn(|c| c.args.first().is_some_and(|a| a == "--machine") && c.display().contains("agent list"), |_| Ok(ok(r#"{"result":{"agents":[{"pane_id":"w2:p1","tab_id":"w2:t1","workspace_id":"w2","cwd":"/remote/alpha","agent":"claude","agent_status":"working","name":"hp-alpha-t-0001"}]}}"#)));
    w.runner.on_fn(|c| c.args.first().is_some_and(|a| a == "--machine") && c.display().contains("pane list"), |_| Ok(ok(r#"{"result":{"panes":[{"pane_id":"w2:p1","tab_id":"w2:t1","workspace_id":"w2","cwd":"/remote/alpha"},{"pane_id":"w3:p1","tab_id":"w3:t1","workspace_id":"w3","cwd":"/remote/beta"}]}}"#)));
    w.runner.on("agent list", ok(r#"{"result":{"agents":[]}}"#));
    w.runner.on("pane list", ok(r#"{"result":{"panes":[]}}"#));
    w.runner.on(
        "machine list --json",
        ok(r#"[{"id":"1","label":"box","target":"worker"}]"#),
    );
    w.runner.on("ssh", ok(""));
    w.runner.on(
        "agent start",
        ok(r#"{"result":{"agent":{"pane_id":"w2:p1","tab_id":"w2:t1","workspace_id":"w2"}}}"#),
    );
    w.runner.on("report-metadata", ok("{}"));
    let ctx = w.ctx();
    let mut memory = steps::Memory::new(&ctx);
    for _ in 0..65 {
        assert!(ticker::tick_for_test(&ctx, &mut memory));
    }
    assert_eq!(thread::load(&a, "t-0001").unwrap().last_state, "working");
    assert!(thread::load(&b, "t-0001").unwrap().launch_attempts > 0);
    let calls = w.runner.calls.borrow();
    let ssh_calls: Vec<_> = calls.iter().filter(|c| c.program == "ssh").collect();
    assert!(
        ssh_calls
            .iter()
            .any(|c| c.display().contains("/remote/alpha"))
    );
    assert!(
        ssh_calls
            .iter()
            .any(|c| c.display().contains("/remote/beta"))
    );
}

#[test]
fn corrupt_inventory_refuses_allocation() {
    let w = World::new();
    let p = w.project("demo", "a.sock");
    let t = w.thread(&p, w.home.path(), |_| {});
    assert_eq!(thread::list(&p).len(), 1);
    std::fs::write(thread::record_path(&p, &t.id), "broken = [").unwrap();
    assert!(thread::load(&p, &t.id).is_err());
    assert!(thread::list_checked(&p).is_err());
    assert!(thread::allocate(&p, |_| {}).is_err());
}

#[test]
fn review_restart_refuses_an_ambiguous_preexisting_branch() {
    let t = thread::Thread {
        id: "t-0001".into(),
        kind: thread::Kind::Worktree,
        status: thread::Status::Failed,
        ..Default::default()
    };
    let error = threads::restart_plan(&t, &thread::Live::default(), true, jiff::Timestamp::now())
        .unwrap_err();
    assert!(error.to_string().contains("half-made worktree"));
}
