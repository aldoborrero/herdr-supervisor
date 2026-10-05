use super::*;

fn amended(title: &str) -> Contract {
    let mut c = contract();
    c.title = title.into();
    c.instructions = " Preserve trailing space and literal $HOME `text` ".into();
    c
}
fn change(task: &str, revision: u64, dependencies: Vec<String>) -> Mutation {
    Mutation::AmendTask {
        task: task.into(),
        expected_revision: revision,
        contract: amended("Revised parser"),
        dependencies,
    }
}
fn predecessors(f: &Fixture, task: &str) -> Vec<String> {
    f.store
        .connection
        .prepare("SELECT predecessor FROM dependencies WHERE task=?1 ORDER BY predecessor")
        .unwrap()
        .query_map([task], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn amend_task_replaces_contract_edges_and_hash_once_preserving_prior_audit() {
    let mut f = Fixture::new();
    let before = f.task();
    let after = f.task();
    let task = f.task();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AddDependency {
            task: task.clone(),
            expected_revision: 1,
            predecessor: before,
        },
    );
    let request = request(&f.supervisor, change(&task, 2, vec![after.clone()]));
    let result = f.store.apply_in_run(&f.run, &request).unwrap();
    assert_eq!(result["revision"], 3);
    assert_eq!(
        result["contract"],
        serde_json::to_value(amended("Revised parser")).unwrap()
    );
    assert_eq!(
        result["contract_hash"],
        digest(&serde_json::to_vec(&amended("Revised parser")).unwrap())
    );
    assert_eq!(predecessors(&f, &task), vec![after]);
    let old: String = f
        .store
        .connection
        .query_row(
            "SELECT doc FROM journal WHERE aggregate_id=?1 AND kind='create_task'",
            [&task],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&old).unwrap()["params"]["contract"],
        serde_json::to_value(contract()).unwrap()
    );
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(f.store.apply_in_run(&f.run, &request).unwrap(), result);
    assert_eq!(
        f.store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM journal WHERE aggregate_id=?1 AND kind='amend_task'",
                [&task],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn amend_task_cycle_foreign_edges_invalid_checks_and_journal_failure_roll_back_everything() {
    let mut f = Fixture::new();
    let a = f.task();
    let b = f.task();
    let c = f.task();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AddDependency {
            task: a.clone(),
            expected_revision: 1,
            predecessor: b.clone(),
        },
    );
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AddDependency {
            task: c.clone(),
            expected_revision: 1,
            predecessor: a.clone(),
        },
    );
    let foreign_run = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "other".into(),
            repo: "/other".into(),
            base_oid: BASE.into(),
            objective: "Other goal".into(),
            policy: policy(),
        },
    );
    let foreign = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateTask {
            run: text(&foreign_run, "id"),
            expected_revision: 1,
            contract: contract(),
            dependencies: vec![],
        },
    );
    let baseline: Task = document(&f.store.connection, "tasks", &a).unwrap();
    for edge in [a.clone(), c, text(&foreign, "id"), "missing-task".into()] {
        assert!(
            f.store
                .apply(&request(&f.supervisor, change(&a, 2, vec![edge])))
                .is_err()
        );
        let current: Task = document(&f.store.connection, "tasks", &a).unwrap();
        assert_eq!(
            serde_json::to_value(&current).unwrap(),
            serde_json::to_value(&baseline).unwrap()
        );
        assert_eq!(predecessors(&f, &a), vec![b.clone()]);
    }
    let mut invalid = amended("Bad check");
    invalid.checks = vec!["unauthorized".into()];
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                Mutation::AmendTask {
                    task: a.clone(),
                    expected_revision: 2,
                    contract: invalid,
                    dependencies: vec![]
                }
            ))
            .is_err()
    );
    f.store.connection.execute_batch("CREATE TRIGGER fail_amend_journal BEFORE INSERT ON journal WHEN NEW.kind='amend_task' BEGIN SELECT RAISE(ABORT,'journal unavailable'); END;").unwrap();
    let req = request(&f.supervisor, change(&a, 2, vec![]));
    assert!(f.store.apply(&req).is_err());
    let current: Task = document(&f.store.connection, "tasks", &a).unwrap();
    assert_eq!(
        serde_json::to_value(&current).unwrap(),
        serde_json::to_value(&baseline).unwrap()
    );
    assert_eq!(predecessors(&f, &a), vec![b]);
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_amend_journal")
        .unwrap();
    assert!(f.store.apply(&req).is_ok());
    assert!(predecessors(&f, &a).is_empty());
}

#[test]
fn amend_task_fences_worker_foreign_scope_live_tasks_and_stale_revisions() {
    let mut f = Fixture::new();
    let task = f.task();
    let other = f.task();
    let (_, worker) = f.dispatch(&other);
    assert!(
        f.store
            .apply(&request(&worker, change(&task, 1, vec![])))
            .is_err()
    );
    let different = call(
        &mut f.store,
        &f.owner,
        Mutation::CreateRun {
            project: "different".into(),
            repo: "/different".into(),
            base_oid: BASE.into(),
            objective: "Other goal".into(),
            policy: policy(),
        },
    );
    assert!(
        f.store
            .apply_in_run(
                &text(&different, "id"),
                &request(&f.owner, change(&task, 1, vec![]))
            )
            .is_err()
    );
    let edit = request(&f.supervisor, change(&task, 1, vec![]));
    let accepted = f.store.apply(&edit).unwrap();
    assert!(
        f.store
            .apply(&request(&f.supervisor, change(&task, 1, vec![])))
            .unwrap_err()
            .to_string()
            .contains("stale revision")
    );
    let other_task: Task = document(&f.store.connection, "tasks", &other).unwrap();
    assert!(
        f.store
            .apply(&request(
                &f.supervisor,
                change(&other, other_task.revision, vec![])
            ))
            .unwrap_err()
            .to_string()
            .contains("only planned")
    );
    // A receipt lookup is allowed after a later edit, but never restores old bytes.
    let mut next = amended("Later revision");
    next.criteria.push("New criterion".into());
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AmendTask {
            task: task.clone(),
            expected_revision: 2,
            contract: next.clone(),
            dependencies: vec![],
        },
    );
    assert_eq!(f.store.apply(&edit).unwrap(), accepted);
    let current: Task = document(&f.store.connection, "tasks", &task).unwrap();
    assert_eq!(current.contract, next);
    assert_eq!(current.revision, 3);
    let run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::Pause {
            run: f.run.clone(),
            expected_revision: run.revision,
        },
    );
    assert!(
        f.store
            .apply(&request(&f.supervisor, change(&task, 3, vec![])))
            .is_err()
    );
    assert_eq!(f.store.apply(&edit).unwrap(), accepted);
}

#[test]
fn amend_task_and_dispatch_race_keep_one_contract_and_exact_worker_order() {
    for _ in 0..8 {
        let mut f = Fixture::new();
        let task = f.task();
        let barrier = Arc::new(Barrier::new(2));
        let mut joins = Vec::new();
        for mutation in [
            change(&task, 1, vec![]),
            Mutation::Dispatch {
                task: task.clone(),
                expected_revision: 1,
                node: "local".into(),
                profile: "auto".into(),
            },
        ] {
            let barrier = barrier.clone();
            let dir = f.store.dir.clone();
            let token = f.supervisor.clone();
            joins.push(std::thread::spawn(move || {
                let mut store = Store::open(&dir).unwrap();
                barrier.wait();
                store.apply(&request(&token, mutation))
            }));
        }
        let outcomes: Vec<_> = joins.into_iter().map(|j| j.join().unwrap()).collect();
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        let current: Task = document(&f.store.connection, "tasks", &task).unwrap();
        if outcomes[0].is_ok() {
            assert_eq!(current.contract, amended("Revised parser"));
            call(
                &mut f.store,
                &f.supervisor,
                Mutation::Dispatch {
                    task: task.clone(),
                    expected_revision: current.revision,
                    node: "local".into(),
                    profile: "auto".into(),
                },
            );
        } else {
            assert_eq!(current.contract, contract());
        }
        let payload: String = f
            .store
            .connection
            .query_row("SELECT payload FROM outbox WHERE kind='execute'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            payload["contract"],
            serde_json::to_value(&current.contract).unwrap()
        );
        assert_eq!(payload["attempt"]["contract_hash"], current.contract_hash);
    }
}
