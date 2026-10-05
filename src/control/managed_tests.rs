use super::*;
use crate::{
    managed,
    popup::{self, RowKind, Section},
    project::{self, Project, Status},
    scenarios::World,
};

async fn server(
    f: &Fixture,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let state = f.store.dir.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(crate::control::api::serve(state.clone(), async {
        let _ = stopped.await;
    }));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !state.join("control.sock").exists() {
            assert!(!task.is_finished());
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    (stop, task)
}

fn binding(f: &Fixture) -> managed::Binding {
    managed::Binding {
        version: 1,
        controller: f.store.dir.clone(),
        credential: f.store.dir.join("admin.token"),
        run: f.run.clone(),
        launch_config: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_worker_focus_uses_configured_routes_and_rechecks_exact_native_identity() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, token) = f.dispatch(&task);
    let r = f.ready(&attempt, &token);
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    let p = Project::load_view(&world.root, "demo").unwrap();
    let cfg = f.dir.path().join("focus-routes.json");
    let agent = json!({"pane_id":r.pane,"tab_id":"tab","workspace_id":r.workspace,"terminal_id":r.terminal,"agent":r.runtime,"agent_session":{"value":r.session}});
    let pane = json!({"pane_id":r.pane,"tab_id":"tab","workspace_id":r.workspace,"terminal_id":r.terminal,"cwd":r.worktree});
    *world.agents.borrow_mut() = json!([agent]).to_string();
    *world.panes.borrow_mut() = json!([pane]).to_string();
    world
        .runner
        .on("agent focus", crate::runner::fake::ok("{}"));
    assert!(crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).is_err());
    for machine in [None, Some("worker-host-id")] {
        std::fs::write(
            &cfg,
            json!({"nodes":{"local":{"socket":"/explicit/herdr.sock","machine":machine}}})
                .to_string(),
        )
        .unwrap();
        crate::managed_focus::configure(&p, &cfg).unwrap();
        crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).unwrap();
        let calls = world.runner.calls.borrow();
        let last = calls
            .iter()
            .rfind(|c| c.display().contains("agent focus"))
            .unwrap();
        assert_eq!(last.args.last().unwrap(), &r.pane);
        assert!(
            last.env
                .iter()
                .any(|(key, value)| key == "HERDR_SOCKET_PATH" && value == "/explicit/herdr.sock")
        );
        assert_eq!(
            last.args.first().map(String::as_str),
            Some(if machine.is_some() {
                "--machine"
            } else {
                "agent"
            })
        );
    }
    assert_eq!(world.runner.count("agent focus"), 2);
    for field in ["pane_id", "workspace_id", "terminal_id", "agent"] {
        let mut changed = agent.clone();
        changed[field] = json!("wrong");
        *world.agents.borrow_mut() = json!([changed]).to_string();
        assert!(
            crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).is_err(),
            "{field}"
        );
    }
    let mut changed = agent.clone();
    changed["agent_session"]["value"] = json!(id());
    *world.agents.borrow_mut() = json!([changed]).to_string();
    assert!(crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).is_err());
    *world.agents.borrow_mut() = json!([agent, agent]).to_string();
    assert!(crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).is_err());
    *world.agents.borrow_mut() = json!([agent]).to_string();
    for field in ["terminal_id", "workspace_id", "cwd"] {
        let mut changed = pane.clone();
        changed[field] = json!("wrong");
        *world.panes.borrow_mut() = json!([changed]).to_string();
        assert!(
            crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch).is_err(),
            "{field}"
        );
    }
    *world.panes.borrow_mut() = json!([pane]).to_string();
    assert!(crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, attempt.epoch + 1).is_err());
    let state = f.store.dir.clone();
    let id = attempt.id.clone();
    let response = json!({"result":{"agents":[agent]}}).to_string();
    let runner = crate::runner::fake::FakeRunner::new();
    runner.on_fn(
        |c| c.display().contains("agent list"),
        move |_| {
            let store = Store::open(&state).unwrap();
            store
                .connection
                .execute("UPDATE attempts SET active=0 WHERE id=?1", [&id])
                .unwrap();
            Ok(crate::runner::fake::ok(&response))
        },
    );
    runner.on(
        "pane list",
        crate::runner::fake::ok(&json!({"result":{"panes":[pane]}}).to_string()),
    );
    let mut ctx = world.ctx();
    ctx.runner = &runner;
    assert!(crate::managed_focus::focus(&ctx, &p, &attempt.id, attempt.epoch).is_err());
    assert_eq!(runner.count("agent list"), 1);
    assert_eq!(runner.count("agent focus"), 0);
    assert_eq!(world.runner.count("agent focus"), 2);
    assert_eq!(world.runner.count("agent start"), 0);
    stop.send(()).unwrap();
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_focus_routes_are_owner_configured_bounded_and_bound_to_the_run() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, token) = f.dispatch(&task);
    f.ready(&attempt, &token);
    assert!(
        crate::control::pagination::focus_attempt(&f.store, &token, &f.run, &attempt.id, 1)
            .is_err()
    );
    assert!(
        crate::control::pagination::focus_attempt(
            &f.store,
            &f.supervisor,
            "foreign",
            &attempt.id,
            1
        )
        .is_err()
    );
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    let p = Project::load_view(&world.root, "demo").unwrap();
    let cfg = f.dir.path().join("focus-routes.json");
    for value in [
        json!({"nodes":{"local":{"socket":"relative"}}}),
        json!({"nodes":{"local":{"socket":"/socket","machine":"--bad value"}}}),
        json!({"nodes":{"local":{"socket":"/socket","extra":true}}}),
    ] {
        std::fs::write(&cfg, value.to_string()).unwrap();
        assert!(crate::managed_focus::configure(&p, &cfg).is_err());
        assert!(!p.state_dir().join("focus.json").exists());
    }
    std::fs::write(&cfg, " ".repeat(65537)).unwrap();
    assert!(crate::managed_focus::configure(&p, &cfg).is_err());
    std::fs::write(
        &cfg,
        json!({"nodes":{"local":{"socket":"/socket"}}}).to_string(),
    )
    .unwrap();
    let link = f.dir.path().join("routes-link");
    std::os::unix::fs::symlink(&cfg, &link).unwrap();
    assert!(crate::managed_focus::configure(&p, &link).is_err());
    crate::managed_focus::configure(&p, &cfg).unwrap();
    let saved = std::fs::read(p.state_dir().join("focus.json")).unwrap();
    let credential = f.store.dir.join("supervisor-view.token");
    secret_file(&credential, f.supervisor.as_bytes()).unwrap();
    let mut other = binding(&f);
    other.credential = credential;
    project::write_json(&p.state_dir().join("managed.json"), &other).unwrap();
    assert!(crate::managed_focus::configure(&p, &cfg).is_err());
    assert_eq!(
        std::fs::read(p.state_dir().join("focus.json")).unwrap(),
        saved
    );
    let mut foreign: Value = serde_json::from_slice(&saved).unwrap();
    foreign["run"] = json!("foreign");
    project::write_json(&p.state_dir().join("focus.json"), &foreign).unwrap();
    assert!(crate::managed_focus::focus(&world.ctx(), &p, &attempt.id, 1).is_err());
    assert_eq!(world.runner.count("agent list"), 0);
    stop.send(()).unwrap();
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_projects_render_controller_records_and_decisions_use_exact_revisions() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
    let world = World::new();
    drop(project::create(&world.root, "demo", "legacy goal", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    assert!(Project::load(&world.root, "demo").is_err());
    let p = Project::load_view(&world.root, "demo").unwrap();
    assert!(p.lock().is_err());
    assert!(managed::overview(&p).unwrap().contains("Fix parser"));
    assert!(managed::summary(&managed::context(&p).unwrap()).contains("1 active attempts"));
    // Files cannot replace the controller view or authorize any scheduler.
    std::fs::write(p.dir().join("TASKS.md"), "# Tasks\n\n- [ ] fake task\n").unwrap();
    std::fs::write(p.dir().join("MEMORY.md"), "unapproved fake memory").unwrap();
    let rows = popup::build(&world.ctx(), Section::Tasks, Some("demo"));
    assert!(rows.iter().any(|r| r.text.contains("Fix parser")));
    assert!(!rows.iter().any(|r| r.text.contains("fake task")));
    let rows = popup::build(&world.ctx(), Section::Threads, Some("demo"));
    let stop_action = rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Managed { actions, .. } => actions.iter().find(|a| a.key == 's'),
            _ => None,
        })
        .unwrap();
    assert!(
        matches!(&stop_action.command,Mutation::StopAttempt{attempt:id,expected_epoch} if id==&attempt.id && *expected_epoch==attempt.epoch)
    );
    let rows = popup::build(&world.ctx(), Section::Settings, Some("demo"));
    let pause = rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Managed { actions, .. } => actions.iter().find(|a| a.label == "pause"),
            _ => None,
        })
        .unwrap()
        .clone();
    f.task(); // Changes the revision after the displayed row was captured.
    assert!(binding(&f).apply("stale-ui-pause", pause.command).is_err());
    crate::lifecycle::set_status(&world.ctx(), "demo", Status::Paused).unwrap();
    assert_eq!(managed::context(&p).unwrap()["run"]["state"], "paused");
    assert_eq!(
        p.status(),
        Status::Active,
        "legacy status remains unchanged"
    );
    crate::lifecycle::set_status(&world.ctx(), "demo", Status::Active).unwrap();
    assert_eq!(managed::context(&p).unwrap()["run"]["state"], "active");
    assert!(!managed::context(&p).unwrap().to_string().contains(&f.owner));
    assert!(
        popup::picker_rows(&world.root)
            .iter()
            .any(|r| r.slug.as_deref() == Some("demo"))
    );
    let _ = stop.send(());
    api.await.unwrap().unwrap();
    let rows = popup::build(&world.ctx(), Section::Tasks, Some("demo"));
    assert!(
        rows.iter()
            .any(|r| r.text.contains("controller unavailable"))
    );
    assert!(!rows.iter().any(|r| r.text.contains("fake task")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_binding_blocks_legacy_effects_and_rejects_foreign_owner_decisions() {
    let mut f = Fixture::new();
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    let foreign = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "other".into(),
            repo: "/other".into(),
            base_oid: BASE.into(),
            objective: "foreign".into(),
            policy: policy(),
        },
    );
    assert!(
        binding(&f)
            .apply(
                "foreign",
                Mutation::Cancel {
                    run: text(&foreign, "id"),
                    expected_revision: 1
                }
            )
            .is_err()
    );
    let foreign_task = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateTask {
            run: text(&foreign, "id"),
            expected_revision: 1,
            contract: contract(),
            dependencies: vec![],
        },
    );
    assert!(
        binding(&f)
            .apply(
                "foreign-task",
                Mutation::Dispatch {
                    task: text(&foreign_task, "id"),
                    expected_revision: 1,
                    node: "local".into(),
                    profile: "auto".into()
                }
            )
            .is_err()
    );
    assert!(
        binding(&f)
            .apply(
                "enroll",
                Mutation::EnrollNode {
                    id: "other".into(),
                    key: Identity::generate().unwrap().did(),
                    max_active: 1
                }
            )
            .is_err()
    );
    assert!(
        crate::coordinator::open(
            &world.ctx(),
            "demo",
            &crate::coordinator::OpenOptions {
                session: Default::default(),
                rebind: false,
                profile: None,
                new: false,
                here: false
            }
        )
        .is_err()
    );
    assert!(crate::threads::stop(&world.ctx(), "demo", "t-0001").is_err());
    assert!(crate::lifecycle::delete(&world.ctx(), "demo", true).is_err());
    let mut memory = crate::steps::Memory::new(&world.ctx());
    std::fs::create_dir_all(world.root.join(".renames")).unwrap();
    std::fs::write(world.root.join(".renames/old.json"),serde_json::to_vec(&json!({"from":"old","to":"demo","requested":"2026-10-05T00:00:00Z","notify":{"socket":"/fixture/herdr.sock","pane":"w1:p1","text":"must never be sent","since":"2026-10-05T00:00:00Z"}})).unwrap()).unwrap();
    crate::ticker::tick_for_test(&world.ctx(), &mut memory);
    crate::ticker::brief_pass_for_test(&world.ctx());
    assert!(
        world.runner.calls.borrow().is_empty(),
        "no Herdr/Git/model commands for a managed project"
    );
    let p = Project::load_view(&world.root, "demo").unwrap();
    std::fs::write(p.state_dir().join("managed.json"), "broken").unwrap();
    assert!(Project::load(&world.root, "demo").is_err());
    assert!(managed::context(&p).is_err());
    assert!(
        popup::build(&world.ctx(), Section::Memory, Some("demo"))
            .iter()
            .any(|r| r.text.contains("controller unavailable"))
    );
    let marker = p.state_dir().join("managed.json");
    std::fs::remove_file(&marker).unwrap();
    std::os::unix::fs::symlink(f.store.dir.join("admin.token"), &marker).unwrap();
    assert!(managed::context(&p).is_err());
    assert!(Project::load(&world.root, "demo").is_err());
    std::fs::remove_file(&marker).unwrap();
    let name = std::ffi::CString::new(marker.to_str().unwrap()).unwrap();
    // A corrupt special-file marker must not hang the UI while opening it.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(managed::context(&p).is_err());
    assert!(Project::load(&world.root, "demo").is_err());
    let _ = stop.send(());
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_memory_actions_preserve_provenance_revision_and_withdrawal() {
    let mut f = Fixture::new();
    let proposal = call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: "parser".into(),
            text: "Read the corpus\u{1b}[2J".into(),
            source_attempt: None,
            evidence: vec!["docs/corpus.md".into()],
        },
    );
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    let rows = popup::build(&world.ctx(), Section::Memory, Some("demo"));
    let (doc, approve) = rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Managed { doc, actions, .. } if doc["id"] == proposal["id"] => {
                Some((doc, actions[0].clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(doc["evidence"], json!(["docs/corpus.md"]));
    binding(&f)
        .apply("approve-from-panel", approve.command.clone())
        .unwrap();
    // Replay must remain possible after the proposal leaves the pending page.
    // Controller idempotency handles it; the view scope must retain provenance.
    binding(&f)
        .apply("approve-from-panel", approve.command)
        .unwrap();
    let rows = popup::build(&world.ctx(), Section::Memory, Some("demo"));
    let withdraw = rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Managed { actions, .. } => actions.iter().find(|a| a.label == "withdraw"),
            _ => None,
        })
        .unwrap();
    binding(&f)
        .apply("withdraw-from-panel", withdraw.command.clone())
        .unwrap();
    let p = Project::load_view(&world.root, "demo").unwrap();
    assert!(
        managed::context(&p).unwrap()["memory"]["topics"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(!managed::display("a\u{1b}[2J\r\nb").contains('\u{1b}'));
    let _ = stop.send(());
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_open_focuses_only_the_exact_observed_native_coordinator() {
    let mut f = Fixture::new();
    let uuid = id();
    call(
        &mut f.store,
        &f.owner,
        Mutation::BindCoordinator {
            run: f.run.clone(),
            expected_revision: 2,
            endpoint: crate::control::coordinator::Endpoint {
                agent: "coord".into(),
                mailer_session: id(),
                mailer_epoch: 1,
                runtime: "codex".into(),
                native_session: uuid.clone(),
            },
        },
    );
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    *world.agents.borrow_mut()=json!([{"pane_id":"w3:p1","tab_id":"w3:t1","workspace_id":"w3","terminal_id":"term3","agent":"codex","agent_session":{"value":uuid}}]).to_string();
    *world.panes.borrow_mut() =
        json!([{"pane_id":"w3:p1","tab_id":"w3:t1","workspace_id":"w3","terminal_id":"term3"}])
            .to_string();
    world
        .runner
        .on("agent focus", crate::runner::fake::ok(r#"{"result":{}}"#));
    let socket = world.home.path().join("herdr.sock");
    std::fs::write(&socket, "").unwrap();
    let options = crate::coordinator::OpenOptions {
        session: crate::paths::SessionFlags {
            socket: Some(socket.clone()),
            session: None,
        },
        rebind: false,
        profile: None,
        new: false,
        here: false,
    };
    crate::coordinator::open(&world.ctx(), "demo", &options).unwrap();
    assert_eq!(world.runner.count("agent focus"), 1);
    assert_eq!(world.runner.count("agent start"), 0);
    assert_eq!(
        crate::overview::project_for_workspace(&world.ctx(), "w3", socket.to_str().unwrap()),
        Some("demo".into())
    );
    let changed = world.agents.borrow().replace(&uuid, &id());
    *world.agents.borrow_mut() = changed;
    assert!(crate::coordinator::open(&world.ctx(), "demo", &options).is_err());
    assert_eq!(world.runner.count("agent focus"), 1);
    let _ = stop.send(());
    api.await.unwrap().unwrap();
}

#[test]
fn project_authority_transition_excludes_existing_operations_and_clones() {
    let world = World::new();
    let p = project::create(&world.root, "demo", "", vec![]).unwrap();
    let clone = p.clone();
    assert!(managed::authority_lease(&p, true).is_err());
    drop(p);
    let raw = Project::load_unlocked(&world.root, "demo").unwrap();
    assert!(managed::authority_lease(&raw, true).is_err());
    drop(clone);
    let exclusive = managed::authority_lease(&raw, true).unwrap();
    std::fs::write(raw.state_dir().join("managed.json"), "broken").unwrap();
    drop(exclusive);
    assert!(Project::load(&world.root, "demo").is_err());
    assert!(Project::load_view(&world.root, "demo").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_priming_keeps_managed_fence_and_retry_repairs_only_the_view() {
    let f = Fixture::new();
    let world = World::new();
    let p = project::create(&world.root, "demo", "", vec![]).unwrap();
    std::fs::create_dir(p.dir().join("AGENTS.md")).unwrap();
    let dir = p.dir();
    drop(p);
    let (stop, api) = server(&f).await;
    assert!(managed::attach(&world.root, "demo", binding(&f)).is_err());
    assert!(Project::load(&world.root, "demo").is_err());
    assert!(dir.join(".state/managed.json").is_file());
    std::fs::remove_dir(dir.join("AGENTS.md")).unwrap();
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    assert!(
        std::fs::read_to_string(dir.join("AGENTS.md"))
            .unwrap()
            .contains(&f.run)
    );
    let _ = stop.send(());
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_open_reserves_once_and_displays_pending_launch_without_legacy_effects() {
    let f = Fixture::new();
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let config = super::coordinator_launch_tests::config(&f);
    let file = f.dir.path().join("launch.json");
    std::fs::write(&file, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut binding = binding(&f);
    binding.launch_config = Some(file);
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding).unwrap();
    let mut options = crate::coordinator::OpenOptions {
        session: Default::default(),
        rebind: false,
        profile: Some("forbidden".into()),
        new: false,
        here: false,
    };
    assert!(
        crate::coordinator::open(&world.ctx(), "demo", &options)
            .unwrap_err()
            .to_string()
            .contains("not authorized")
    );
    options.profile = None;
    crate::coordinator::open(&world.ctx(), "demo", &options).unwrap();
    crate::coordinator::open(&world.ctx(), "demo", &options).unwrap();
    options.new = true;
    assert!(crate::coordinator::open(&world.ctx(), "demo", &options).is_err());
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT COUNT(*) FROM coordinator_launches", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let rows = popup::build(&world.ctx(), Section::Settings, Some("demo"));
    assert!(
        rows.iter()
            .any(|r| r.text.contains("Coordinator codex · reserved"))
    );
    assert!(
        world.runner.calls.borrow().is_empty(),
        "managed open submits intent, never runs a legacy command"
    );
    stop.send(()).unwrap();
    api.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn managed_task_form_applies_one_exact_contract_with_dependencies_and_rejects_stale_creation()
{
    let mut f = Fixture::new();
    let predecessor = f.task();
    let world = World::new();
    drop(project::create(&world.root, "demo", "", vec![]).unwrap());
    let (stop, api) = server(&f).await;
    managed::attach(&world.root, "demo", binding(&f)).unwrap();
    let p = Project::load_view(&world.root, "demo").unwrap();
    let context = managed::context(&p).unwrap();
    let mut form = crate::managed_task_form::Form::new("demo", &context).unwrap();
    crate::managed_task_form::tests::fill(&mut form);
    form.fields[6].value = serde_json::to_string(&vec![&predecessor]).unwrap();
    let args = form.prepare().unwrap();
    let operation = args.args[4].clone();
    let command: Mutation = serde_json::from_str(&args.request).unwrap();
    let first = binding(&f).apply(&operation, command.clone()).unwrap();
    let replay = binding(&f).apply(&operation, command).unwrap();
    assert_eq!(first, replay);
    assert_eq!(
        first["contract"]["instructions"],
        "Preserve literal $HOME and `text`"
    );
    let task_id = first["id"].as_str().unwrap();
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        f.store
            .connection
            .query_row(
                "SELECT predecessor FROM dependencies WHERE task=?1",
                [task_id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        predecessor
    );
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT COUNT(*) FROM attempts", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    let mut stale = crate::managed_task_form::Form::new("demo", &context).unwrap();
    crate::managed_task_form::tests::fill(&mut stale);
    stale.fields[0].value = "Concurrent task".into();
    stale.fields[6].value = "[]".into();
    let args = stale.prepare().unwrap();
    let command = serde_json::from_str(&args.request).unwrap();
    let error = binding(&f).apply(&args.args[4], command).unwrap_err();
    assert!(error.to_string().contains("stale revision"), "{error:#}");
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        2
    );
    let _ = stop.send(());
    api.await.unwrap().unwrap();
}
