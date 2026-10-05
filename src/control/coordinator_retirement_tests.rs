use super::*;
use crate::control::coordinator_retirement::{self as retirement, Request};
use rusqlite::params;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

struct Retiring {
    f: Fixture,
    world: World,
    spaces: Rc<RefCell<String>>,
    idle: Rc<Cell<bool>>,
    close_effect: Rc<Cell<bool>>,
    request: Request,
    home: std::path::PathBuf,
}

// Called by the Codex/Claude/Pi launcher regressions after their real harmless
// native child and wrapper have exited. Herdr/mailer/model identity is simulated.
pub(super) fn retire_exited(f: &mut Fixture, world: &World, target: &str) {
    let p = plan(f, target);
    let home = launch::files(&f.store.dir, &p).join("home");
    let instructions = std::fs::read(home.join("AGENTS.md")).unwrap();
    let open = Rc::new(Cell::new(true));
    let observation = open.clone();
    world.runner.on_fn(
        |c| c.display().contains("workspace list"),
        move |_| {
            Ok(ok(if observation.get() {
                r#"{"result":{"workspaces":[{"workspace_id":"w1","pane_count":1}]}}"#
            } else {
                r#"{"result":{"workspaces":[]}}"#
            }))
        },
    );
    let (observation, panes) = (open, world.panes.clone());
    world.runner.on_fn(
        |c| c.display().contains("workspace close"),
        move |_| {
            observation.set(false);
            *panes.borrow_mut() = "[]".into();
            Ok(timeout())
        },
    );
    let r = Request {
        operation: id(),
        launch: target.into(),
        expected_generation: p.generation,
        expected_binding: p.pane.unwrap(),
        processes_confirmed_stopped: true,
        retain_files_close_workspace: true,
        reason: "Real harmless fixture child and wrapper joined".into(),
    };
    let receipt = retirement::retire_with(&mut f.store, &f.owner, &r, &world.runner).unwrap();
    assert_eq!(receipt["state"], "closed");
    assert_eq!(
        retirement::retire_with(&mut f.store, &f.owner, &r, &world.runner).unwrap(),
        receipt
    );
    assert_eq!(world.runner.count("workspace close"), 1);
    assert_eq!(std::fs::read(home.join("AGENTS.md")).unwrap(), instructions);
}

impl Retiring {
    fn new() -> Self {
        let mut f = Fixture::new();
        let cfg = config(&f);
        let target = start(&mut f, cfg);
        let p = plan(&f, &target);
        let home = launch::files(&f.store.dir, &p).join("home");
        let world = World::new();
        let idle = Rc::new(Cell::new(true));
        let observed_idle = idle.clone();
        world.runner.on_fn(|c|c.display().contains("pane process-info"), move |_| {
            Ok(ok(if observed_idle.get() {r#"{"result":{"process_info":{"shell_pid":100,"foreground_process_group_id":100,"foreground_processes":[{"pid":100}]}}}"#}else{r#"{"result":{"process_info":{}}}"#}))
        });
        shell(&world, &home, false);
        assert_eq!(
            launch::advance(&mut f.store, &target, &world.runner).unwrap()["state"],
            "launching"
        );
        let p = plan(&f, &target);
        assert!(p.launch_started && !p.wrapper_started);
        let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        call(
            &mut f.store,
            &f.owner,
            Mutation::BindSupervisor {
                run: f.run.clone(),
                expected_revision: run.revision,
            },
        );
        assert_eq!(
            launch::advance(&mut f.store, &target, &world.runner).unwrap()["state"],
            "exited"
        );
        let request = Request {
            operation: id(),
            launch: target,
            expected_generation: p.generation,
            expected_binding: p.pane.unwrap(),
            processes_confirmed_stopped: true,
            retain_files_close_workspace: true,
            reason: "All fixture users are stopped; keep their files".into(),
        };
        let spaces = Rc::new(RefCell::new(
            json!([{"workspace_id":"w1","pane_count":1}]).to_string(),
        ));
        let list = spaces.clone();
        world.runner.on_fn(
            |c| c.display().contains("workspace list"),
            move |_| {
                Ok(ok(&format!(
                    "{{\"result\":{{\"workspaces\":{}}}}}",
                    list.borrow()
                )))
            },
        );
        let (panes, list) = (world.panes.clone(), spaces.clone());
        let close_effect = Rc::new(Cell::new(true));
        let effect = close_effect.clone();
        world.runner.on_fn(
            |c| c.display().contains("workspace close"),
            move |c| {
                assert!(
                    c.display().ends_with("workspace close w1"),
                    "{}",
                    c.display()
                );
                if !effect.get() {
                    return Ok(timeout());
                }
                *panes.borrow_mut() = "[]".into();
                *list.borrow_mut() = "[]".into();
                Ok(ok(r#"{"result":{}}"#))
            },
        );
        std::fs::write(home.join("notes.md"), "Preserve unsaved coordinator notes").unwrap();
        Self {
            f,
            world,
            spaces,
            idle,
            close_effect,
            request,
            home,
        }
    }
    fn retire(&mut self) -> anyhow::Result<Value> {
        retirement::retire_with(
            &mut self.f.store,
            &self.f.owner,
            &self.request,
            &self.world.runner,
        )
    }
    fn count(&self) -> i64 {
        self.f
            .store
            .connection
            .query_row("SELECT count(*) FROM coordinator_retirements", [], |r| {
                r.get(0)
            })
            .unwrap()
    }
}

#[test]
fn coordinator_retirement_closes_only_owned_workspace_preserves_files_and_replay() {
    let mut x = Retiring::new();
    let receipt = x.retire().unwrap();
    assert_eq!(receipt["state"], "closed");
    assert_eq!(receipt["files_deleted"], false);
    assert_eq!(
        std::fs::read_to_string(x.home.join("notes.md")).unwrap(),
        "Preserve unsaved coordinator notes"
    );
    assert!(x.home.join("AGENTS.md").exists());
    assert_eq!(x.retire().unwrap(), receipt);
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(x.retire().unwrap(), receipt);
    assert_eq!(x.world.runner.count("workspace close"), 1);
    assert_eq!(
        launch::summaries(&x.f.store.connection, &x.f.run).unwrap()[0]["workspace_retirement"],
        "closed"
    );
    assert!(
        retirement::retire_with(&mut x.f.store, &x.f.supervisor, &x.request, &x.world.runner)
            .is_err()
    );
    x.request.reason.push('!');
    assert!(x.retire().is_err());
}

#[test]
fn coordinator_retirement_requires_retired_authority_exact_exit_and_confirmations() {
    let mut x = Retiring::new();
    x.request.processes_confirmed_stopped = false;
    assert!(x.retire().is_err());
    x.request.processes_confirmed_stopped = true;
    x.request.retain_files_close_workspace = false;
    assert!(x.retire().is_err());
    x.request.retain_files_close_workspace = true;
    x.request.expected_generation += 1;
    assert!(x.retire().is_err());
    x.request.expected_generation -= 1;
    x.f.store
        .connection
        .execute("UPDATE coordinator_launches SET state='bound'", [])
        .unwrap();
    assert!(x.retire().is_err());
    x.f.store
        .connection
        .execute("UPDATE coordinator_launches SET state='exited'", [])
        .unwrap();
    x.f.store
        .connection
        .execute(
            "UPDATE principals SET active=1 WHERE role='supervisor' AND generation=?1",
            [x.request.expected_generation],
        )
        .unwrap();
    assert!(x.retire().is_err());
    x.f.store
        .connection
        .execute(
            "UPDATE principals SET active=0 WHERE role='supervisor' AND generation=?1",
            [x.request.expected_generation],
        )
        .unwrap();
    let mut p = plan(&x.f, &x.request.launch);
    p.wrapper_started = true;
    x.f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET doc=?1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    assert!(x.retire().is_err());
    let mut receipt = json!({"launch":x.request.launch,"generation":x.request.expected_generation,"scope":"native_process","reason":"exit"});
    x.f.store
        .connection
        .execute(
            "INSERT INTO coordinator_processes VALUES (?1,?2,'running',?3)",
            params![
                x.request.launch,
                x.request.expected_generation,
                receipt.to_string()
            ],
        )
        .unwrap();
    assert!(x.retire().is_err());
    receipt["launch"] = json!(id());
    x.f.store
        .connection
        .execute(
            "UPDATE coordinator_processes SET state='exited',doc=?1",
            [receipt.to_string()],
        )
        .unwrap();
    assert!(x.retire().is_err());
    assert_eq!(x.count(), 0);
    assert_eq!(x.world.runner.count("workspace close"), 0);
    receipt["launch"] = json!(x.request.launch);
    x.f.store
        .connection
        .execute(
            "UPDATE coordinator_processes SET doc=?1",
            [receipt.to_string()],
        )
        .unwrap();
    assert_eq!(x.retire().unwrap()["state"], "closed");
}

#[test]
fn coordinator_retirement_refuses_reused_busy_shared_or_substituted_resources() {
    let mut x = Retiring::new();
    let original = x.world.panes.borrow().clone();
    let spaces = x.spaces.borrow().clone();
    *x.spaces.borrow_mut() = json!([{"workspace_id":"w1","pane_count":2}]).to_string();
    assert!(x.retire().is_err());
    *x.spaces.borrow_mut() = spaces;
    for field in ["pane_id", "terminal_id", "cwd", "foreground_cwd"] {
        let mut panes: Value = serde_json::from_str(&original).unwrap();
        panes[0][field] = json!("foreign");
        *x.world.panes.borrow_mut() = panes.to_string();
        assert!(x.retire().is_err(), "{field}");
    }
    *x.world.panes.borrow_mut() = original.clone();
    let mut panes: Value = serde_json::from_str(&original).unwrap();
    panes
        .as_array_mut()
        .unwrap()
        .push(json!({"workspace_id":"w1","pane_id":"other","tab_id":"other"}));
    *x.world.panes.borrow_mut() = panes.to_string();
    assert!(x.retire().is_err());
    *x.world.panes.borrow_mut() = original;
    observed(
        &x.world,
        &plan(&x.f, &x.request.launch),
        &id(),
        "t1",
        &x.home,
    );
    assert!(x.retire().is_err());
    *x.world.agents.borrow_mut() = "[]".into();
    x.idle.set(false);
    assert!(x.retire().is_err());
    assert_eq!(x.count(), 0);
    assert_eq!(x.world.runner.count("workspace close"), 0);
    let backup = x.home.with_file_name("retained");
    std::fs::rename(&x.home, &backup).unwrap();
    std::os::unix::fs::symlink(&backup, &x.home).unwrap();
    assert!(x.retire().is_err());
    assert!(backup.join("notes.md").exists());
}

#[test]
fn coordinator_retirement_rolls_back_audit_failure_and_reconciles_lost_completion() {
    let mut x = Retiring::new();
    x.f.store.connection.execute_batch("CREATE TRIGGER fail_prepare BEFORE INSERT ON journal WHEN NEW.kind='coordinator_retirement_prepared' BEGIN SELECT RAISE(FAIL,'lost audit'); END;").unwrap();
    assert!(x.retire().is_err());
    assert_eq!(x.count(), 0);
    assert_eq!(x.world.runner.count("workspace close"), 0);
    x.f.store.connection.execute_batch("DROP TRIGGER fail_prepare; CREATE TRIGGER fail_complete BEFORE INSERT ON journal WHEN NEW.kind='coordinator_retirement_completed' BEGIN SELECT RAISE(FAIL,'lost audit'); END;").unwrap();
    assert!(x.retire().is_err());
    assert_eq!(x.world.runner.count("workspace close"), 1);
    let state: String =
        x.f.store
            .connection
            .query_row("SELECT state FROM coordinator_retirements", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(state, "closing");
    x.f.store
        .connection
        .execute_batch("DROP TRIGGER fail_complete")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(x.retire().unwrap()["state"], "closed");
    assert_eq!(x.world.runner.count("workspace close"), 1);
    assert_eq!(
        x.f.store
            .connection
            .query_row(
                "SELECT count(*) FROM journal WHERE kind='coordinator_retirement_completed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn coordinator_retirement_uncertain_close_never_repeats_or_closes_recreated_resources() {
    let mut x = Retiring::new();
    x.close_effect.set(false);
    let result = x.retire().unwrap();
    assert_eq!(result["state"], "closing");
    assert!(result["workspace_closed"].is_null());
    assert_eq!(x.retire().unwrap()["state"], "closing");
    *x.world.panes.borrow_mut()=json!([{"workspace_id":"w1","pane_id":"new","terminal_id":"new","tab_id":"new","cwd":"/elsewhere"}]).to_string();
    assert_eq!(x.retire().unwrap()["state"], "closing");
    assert_eq!(x.world.runner.count("workspace close"), 1);
    let mut p = plan(&x.f, &x.request.launch);
    let original = p.config.herdr_socket.clone();
    p.config.herdr_socket = "/different/socket".into();
    x.f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET doc=?1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    assert!(x.retire().is_err());
    p.config.herdr_socket = original;
    x.f.store
        .connection
        .execute(
            "UPDATE coordinator_launches SET doc=?1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    // Separate operator fixture cleanup allows observation-only reconciliation.
    *x.world.panes.borrow_mut() = "[]".into();
    *x.spaces.borrow_mut() = "[]".into();
    assert_eq!(x.retire().unwrap()["state"], "closed");
    assert_eq!(x.world.runner.count("workspace close"), 1);
}

#[test]
fn coordinator_retirement_migration_preserves_launch_and_lock_fences_effects() {
    let mut x = Retiring::new();
    let original = serde_json::to_string(&plan(&x.f, &x.request.launch)).unwrap();
    x.f.store
        .connection
        .execute_batch("DROP TABLE task_questions; DROP TABLE artifact_retirements; DROP TABLE coordinator_retirements; PRAGMA user_version=12;")
        .unwrap();
    x.f.store = Store::open(&x.f.store.dir).unwrap();
    assert_eq!(
        serde_json::to_string(&plan(&x.f, &x.request.launch)).unwrap(),
        original
    );
    let lease = crate::control::api::lock(&x.f.store.dir, "coordinator-launch.lock").unwrap();
    assert!(x.retire().is_err());
    assert_eq!(x.count(), 0);
    drop(lease);
    assert_eq!(x.retire().unwrap()["state"], "closed");
}
