use super::*;
use crate::runner::{Cmd, RealRunner, Runner};

#[test]
fn wrapper_claim_and_initial_process_receipt_roll_back_together() {
    for fault in [
        "CREATE TRIGGER fail_initial_process BEFORE INSERT ON coordinator_processes BEGIN SELECT RAISE(ABORT,'process receipt write failure'); END;",
        "CREATE TRIGGER fail_initial_process BEFORE UPDATE ON coordinator_launches WHEN json_extract(NEW.doc,'$.wrapper_started') BEGIN SELECT RAISE(ABORT,'wrapper claim write failure'); END;",
        "CREATE TABLE claim_commit_fault (principal TEXT REFERENCES principals(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER fail_initial_process AFTER INSERT ON coordinator_processes BEGIN INSERT INTO claim_commit_fault VALUES ('missing-principal'); END;",
    ] {
        let mut f = Fixture::new();
        let mut cfg = config(&f);
        cfg.codex_binary = stub(&f);
        let target = start(&mut f, cfg);
        let home = launch::files(&f.store.dir, &plan(&f, &target)).join("home");
        let world = World::new();
        shell(&world, &home, false);
        launch::advance(&mut f.store, &target, &world.runner).unwrap();
        f.store.connection.execute_batch(fault).unwrap();
        let mut wrapper = child(&f, &target);
        assert!(!wrapper.wait().success());
        assert!(
            !f.dir.path().join("started").exists(),
            "no native child was spawned"
        );
        assert!(!plan(&f, &target).wrapper_started);
        let count: u64 = f
            .store
            .connection
            .query_row("SELECT count(*) FROM coordinator_processes", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        f.store
            .connection
            .execute_batch("DROP TRIGGER fail_initial_process")
            .unwrap();
        f.store = Store::open(&f.store.dir).unwrap();
        let r: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        call(
            &mut f.store,
            &f.owner,
            Mutation::BindSupervisor {
                run: f.run.clone(),
                expected_revision: r.revision,
            },
        );
        assert_eq!(
            launch::advance(&mut f.store, &target, &world.runner).unwrap()["state"],
            "exited"
        );
        let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
        let replacement = f.store.apply(&request(
            &f.owner,
            Mutation::StartCoordinator {
                run: f.run.clone(),
                expected_revision: run.revision,
                profile: "auto".into(),
                config: config(&f),
            },
        ));
        assert!(
            replacement.is_ok(),
            "a failed pre-spawn SQL insert permanently blocks replacement even after owner fencing"
        );
    }
}

#[test]
fn pane_run_proven_spawn_failure_retries_after_restart() {
    use std::{cell::Cell, rc::Rc};
    let mut f = Fixture::new();
    let cfg = config(&f);
    let target = start(&mut f, cfg);
    let world = World::new();
    let repaired = Rc::new(Cell::new(false));
    let ready = repaired.clone();
    let missing = f.dir.path().join("missing-herdr");
    world.runner.on_fn(
        |c| c.display().contains("pane run"),
        move |_| {
            if ready.get() {
                Ok(ok(r#"{"result":{}}"#))
            } else {
                RealRunner.run(&Cmd::new(missing.to_str().unwrap(), Duration::from_secs(1)))
            }
        },
    );
    let home = launch::files(&f.store.dir, &plan(&f, &target)).join("home");
    shell(&world, &home, false);
    let first = launch::advance(&mut f.store, &target, &world.runner);
    assert!(
        first
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("retry is safe")
    );
    assert!(!plan(&f, &target).launch_started);
    assert_eq!(
        launch::summaries(&f.store.connection, &f.run).unwrap()[0]["launch_result"]["status"],
        "not_started"
    );
    assert!(!plan(&f, &target).wrapper_started);
    repaired.set(true);
    f.store = Store::open(&f.store.dir).unwrap();
    let second = launch::advance(&mut f.store, &target, &world.runner);
    assert!(second.is_ok());
    assert_eq!(world.runner.count("workspace create"), 1);
    assert_eq!(
        launch::summaries(&f.store.connection, &f.run).unwrap()[0]["launch_result"]["status"],
        "submitted"
    );
    assert_eq!(
        world.runner.count("pane run"),
        2,
        "known pre-spawn failure was discarded and never retried"
    );
}

#[test]
fn pane_run_uncertain_outcomes_keep_diagnostics_and_never_retry() {
    use crate::runner::fake::fail;
    for mode in ["timeout", "nonzero", "server_not_started", "runner_error"] {
        let mut f = Fixture::new();
        let cfg = config(&f);
        let target = start(&mut f, cfg);
        let p = plan(&f, &target);
        let home = launch::files(&f.store.dir, &p).join("home");
        let world = World::new();
        let message = format!("failure-{mode} {}", p.credential);
        world.runner.on_fn(
            |c| c.display().contains("pane run"),
            move |_| match mode {
                "timeout" => Ok(timeout()),
                "nonzero" => Ok(fail(1, &message)),
                "server_not_started" => Ok(fail(
                    1,
                    &json!({"error":{"code":"not_started","message":message}}).to_string(),
                )),
                _ => Err(anyhow::anyhow!(message.clone())),
            },
        );
        shell(&world, &home, false);
        assert_eq!(
            launch::advance(&mut f.store, &target, &world.runner).unwrap()["launch_result"]["status"],
            "uncertain"
        );
        for _ in 0..2 {
            f.store = Store::open(&f.store.dir).unwrap();
            let error = launch::advance(&mut f.store, &target, &world.runner)
                .unwrap_err()
                .to_string();
            assert!(error.contains(if mode == "timeout" { "timed out" } else { mode }));
            assert!(!error.contains(&p.credential));
            let page = crate::control::pagination::page(
                &f.store,
                &f.owner,
                &f.run,
                crate::control::pagination::Collection::CoordinatorLaunches,
                None,
                1,
            )
            .unwrap();
            assert_eq!(
                page["items"][0]["value"]["launch_result"]["status"],
                "uncertain"
            );
            assert!(!page.to_string().contains(&p.credential));
        }
        assert!(plan(&f, &target).launch_started);
        assert_eq!(world.runner.count("pane run"), 1);
        assert_eq!(world.runner.count("workspace create"), 1);
    }
}

#[test]
fn pane_run_failed_result_commit_keeps_the_intent_uncertain() {
    for no_spawn in [false, true] {
        let mut f = Fixture::new();
        let cfg = config(&f);
        let target = start(&mut f, cfg);
        let world = World::new();
        let missing = f.dir.path().join("missing-herdr");
        world.runner.on_fn(
            |c| c.display().contains("pane run"),
            move |_| {
                if no_spawn {
                    RealRunner.run(&Cmd::new(missing.to_str().unwrap(), Duration::from_secs(1)))
                } else {
                    Ok(ok(r#"{"result":{}}"#))
                }
            },
        );
        let home = launch::files(&f.store.dir, &plan(&f, &target)).join("home");
        shell(&world, &home, false);
        f.store.connection.execute_batch("CREATE TRIGGER fail_launch_result BEFORE UPDATE ON coordinator_launches WHEN json_extract(NEW.doc,'$.launch_result.status') IS NOT NULL BEGIN SELECT RAISE(ABORT,'launch result write failure'); END;").unwrap();
        assert!(
            launch::advance(&mut f.store, &target, &world.runner)
                .unwrap_err()
                .to_string()
                .contains("launch result write failure")
        );
        f.store
            .connection
            .execute_batch("DROP TRIGGER fail_launch_result")
            .unwrap();
        f.store = Store::open(&f.store.dir).unwrap();
        assert!(plan(&f, &target).launch_started);
        assert!(plan(&f, &target).launch_result.is_none());
        assert!(
            launch::advance(&mut f.store, &target, &world.runner)
                .unwrap_err()
                .to_string()
                .contains("no durable pane run result")
        );
        assert_eq!(world.runner.count("pane run"), 1);
    }
}

#[test]
fn pipe_timeout_releases_launch_fence_for_owner_retirement() {
    let mut f = Fixture::new();
    let cfg = config(&f);
    let target = start(&mut f, cfg);
    let world = World::new();
    world.runner.on_fn(
        |c| c.display().contains("workspace create"),
        |_| {
            RealRunner
                .run(&Cmd::new("sh", Duration::from_millis(50)).args(["-c", "sleep 2 & wait"]))
        },
    );
    let at = Instant::now();
    assert!(launch::advance(&mut f.store, &target, &world.runner).is_err());
    assert!(at.elapsed() < Duration::from_secs(1));
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
    assert_eq!(world.runner.count("workspace create"), 1);
}

#[test]
fn legacy_retirement_hash_survives_new_optional_plan_fields() {
    use crate::control::{coordinator_retirement as retirement, model::digest};
    for nullable in [false, true] {
        for phase in ["prepared", "closing"] {
            let mut f = Fixture::new();
            let cfg = config(&f);
            let target = start(&mut f, cfg);
            let world = World::new();
            let home = launch::files(&f.store.dir, &plan(&f, &target)).join("home");
            shell(&world, &home, false);
            launch::advance(&mut f.store, &target, &world.runner).unwrap();
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
            let mut p = plan(&f, &target);
            p.workspace_result = None;
            p.launch_result = None;
            // Preserve struct field order exactly as serialized before d71052b. JSON
            // Value would sort the fields and manufacture a different, invalid fixture.
            let modern = serde_json::to_string(&p).unwrap();
            let legacy = if nullable {
                modern
            } else {
                modern.replace(",\"workspace_result\":null", "")
            };
            assert_eq!(legacy.contains("workspace_result"), nullable);
            let scope = format!("{{\"plan\":{legacy},\"process\":null}}");
            let hash = digest(scope.as_bytes());
            let req = retirement::Request {
                operation: id(),
                launch: target.clone(),
                expected_generation: p.generation,
                expected_binding: p.pane.unwrap(),
                processes_confirmed_stopped: true,
                retain_files_close_workspace: true,
                reason: "Review legacy close replay".into(),
            };
            f.store
                .connection
                .execute(
                    "UPDATE coordinator_launches SET doc=?2 WHERE id=?1",
                    rusqlite::params![target, &legacy],
                )
                .unwrap();
            f.store
                .connection
                .execute(
                    "INSERT INTO coordinator_retirements VALUES (?1,?2,?3,?4,?6,NULL,?5,?5)",
                    rusqlite::params![
                        target,
                        req.operation,
                        serde_json::to_string(&req).unwrap(),
                        &hash,
                        now(),
                        phase
                    ],
                )
                .unwrap();
            use std::{cell::RefCell, rc::Rc};
            let spaces = Rc::new(RefCell::new(if phase == "closing" {
                "[]".to_owned()
            } else {
                json!([{"workspace_id":"w1","pane_count":1}]).to_string()
            }));
            if phase == "closing" {
                *world.panes.borrow_mut() = "[]".into();
            }
            let observed = spaces.clone();
            world.runner.on_fn(
                |c| c.display().contains("workspace list"),
                move |_| {
                    Ok(ok(&format!(
                        "{{\"result\":{{\"workspaces\":{}}}}}",
                        observed.borrow()
                    )))
                },
            );
            let panes = world.panes.clone();
            world.runner.on_fn(
                |c| c.display().contains("workspace close"),
                move |_| {
                    *panes.borrow_mut() = "[]".into();
                    *spaces.borrow_mut() = "[]".into();
                    Ok(ok(r#"{"result":{}}"#))
                },
            );
            f.store = Store::open(&f.store.dir).unwrap();
            // Compatibility accepts an exact old projection, never changed authority
            // or newly added evidence under an old digest.
            for (field, value) in [
                ("credential", json!("changed-credential")),
                (
                    "workspace_result",
                    json!({"status":"uncertain","error":"new evidence"}),
                ),
                ("launch_result", json!({"status":"submitted"})),
            ] {
                let mut changed: Value = serde_json::from_str(&legacy).unwrap();
                changed[field] = value;
                f.store
                    .connection
                    .execute(
                        "UPDATE coordinator_launches SET doc=?2 WHERE id=?1",
                        rusqlite::params![target, changed.to_string()],
                    )
                    .unwrap();
                assert!(
                    retirement::retire_with(&mut f.store, &f.owner, &req, &world.runner)
                        .unwrap_err()
                        .to_string()
                        .contains("authority changed")
                );
                assert_eq!(world.runner.count("workspace close"), 0);
            }
            f.store
                .connection
                .execute(
                    "UPDATE coordinator_launches SET doc=?2 WHERE id=?1",
                    rusqlite::params![target, legacy],
                )
                .unwrap();
            let result =
                retirement::retire_with(&mut f.store, &f.owner, &req, &world.runner).unwrap();
            assert_eq!(result["state"], "closed");
            assert_eq!(
                world.runner.count("workspace close"),
                usize::from(phase == "prepared")
            );
            let retained: String = f
                .store
                .connection
                .query_row(
                    "SELECT scope_hash FROM coordinator_retirements WHERE launch=?1",
                    [&target],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(retained, hash);
            f.store = Store::open(&f.store.dir).unwrap();
            assert_eq!(
                retirement::retire_with(&mut f.store, &f.owner, &req, &world.runner).unwrap(),
                result
            );
        }
    }
}
