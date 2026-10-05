use super::*;
use crate::control::{
    executor::CheckoutPlan,
    node::NodeStore,
    registration_recovery::recover,
    runtime::{LaunchPlan, RuntimeConfig},
};

fn prepared() -> (Fixture, NodeStore, Attempt, LaunchPlan) {
    let mut f = Fixture::new();
    let mut run: Run = document(&f.store.connection, "runs", &f.run).unwrap();
    run.policy.profiles.get_mut("auto").unwrap().runtime = "pi".into();
    save_run(&f.store.connection, &run).unwrap();
    let task = f.task();
    let (attempt, _) = f.dispatch(&task);
    let mut node = node_store(&f);
    let key = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    let effect = f.store.pending(&f.node, "local").unwrap().remove(0);
    node.receive(&f.store.prepare_wire(&effect.id, &key).unwrap())
        .unwrap();
    let p = LaunchPlan {
        checkout: CheckoutPlan {
            repo: f.dir.path().join("repo"),
            common_dir: f.dir.path().join("repo/.git"),
            worktree: f.dir.path().join("checkout"),
            branch: "owned".into(),
            base_oid: BASE.into(),
            socket: f.dir.path().join("herdr.sock"),
            herdr_binary: "herdr".into(),
            creation_started: true,
            binding: None,
        },
        config: RuntimeConfig {
            supervisor_binary: "/unused/supervisor".into(),
            mailer_state: f.dir.path().join("mailer"),
            mailer_admin_credential: f.dir.path().join("mailer-admin"),
            node_credential: node.dir.join("node-token"),
            codex_binary: "/unused/codex".into(),
            mailer_runtime_binary: "/unused/runtime".into(),
            claude_development_channel: false,
        },
        expected_session: Some(id()),
        registration_started: true,
        registration: None,
        launch_started: false,
        wrapper_started: false,
        wrapper_pid: None,
        bind_event: None,
        bootstrap: None,
    };
    node.connection
        .execute(
            "UPDATE jobs SET state='launching',checkpoint=?2 WHERE id=?1",
            rusqlite::params![attempt.id, serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    (f, node, attempt, p)
}
fn plan(node: &NodeStore, id: &str) -> LaunchPlan {
    let text: String = node
        .connection
        .query_row("SELECT checkpoint FROM jobs WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn registration_recovery_node_is_durable_idempotent_and_preserves_original_assignment() {
    let (f, node, a, before) = prepared();
    let original: String = node
        .connection
        .query_row("SELECT doc FROM jobs WHERE id=?1", [&a.id], |r| r.get(0))
        .unwrap();
    let receipt = recover(&node.dir, true, &a.id, a.epoch, "recover").unwrap();
    assert!(!plan(&node, &a.id).registration_started);
    assert_eq!(plan(&node, &a.id).expected_session, before.expected_session);
    assert_eq!(plan(&node, &a.id).checkout, before.checkout);
    drop(node);
    let node = node_store(&f);
    assert_eq!(
        recover(&node.dir, true, &a.id, a.epoch, "recover").unwrap(),
        receipt
    );
    assert!(recover(&node.dir, true, &a.id, a.epoch + 1, "recover").is_err());
    assert!(
        recover(&node.dir, true, &a.id, a.epoch, "another").is_err(),
        "no repeated authorization before the next failed call"
    );
    let saved: String = node
        .connection
        .query_row("SELECT doc FROM jobs WHERE id=?1", [&a.id], |r| r.get(0))
        .unwrap();
    assert_eq!(saved, original);
    let mut p = plan(&node, &a.id);
    p.registration_started = true;
    p.launch_started = true;
    node.connection
        .execute(
            "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
            rusqlite::params![a.id, serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    assert_eq!(
        recover(&node.dir, true, &a.id, a.epoch, "recover").unwrap(),
        receipt
    );
    assert!(plan(&node, &a.id).launch_started);
    assert!(plan(&node, &a.id).registration_started);
    assert!(recover(&node.dir, true, &a.id, a.epoch, "another").is_err());
    assert!(
        node.pending().unwrap().is_empty(),
        "local authorization queues no controller result"
    );
}

#[test]
fn registration_recovery_node_rejects_process_files_stale_epoch_and_launch_markers() {
    let (_f, node, a, p) = prepared();
    let recover = || recover(&node.dir, true, &a.id, a.epoch, "recover");
    assert!(
        super::super::registration_recovery::recover(&node.dir, true, &a.id, a.epoch + 1, "wrong")
            .is_err()
    );
    let lease = crate::control::api::lock(&node.dir, "execution.lock").unwrap();
    assert!(recover().is_err());
    drop(lease);
    let dir = node.dir.join("workers").join(&a.id);
    std::fs::create_dir_all(&dir).unwrap();
    for name in [
        "registration.json",
        "mailer.token",
        "mailer.json",
        "native-receipts",
    ] {
        let file = dir.join(name);
        std::fs::write(&file, "partial evidence").unwrap();
        assert!(recover().is_err());
        std::fs::remove_file(file).unwrap();
    }
    node.connection
        .execute(
            "INSERT INTO runtime_processes VALUES (?1,?2,'spawn_started','{}')",
            rusqlite::params![a.id, a.epoch],
        )
        .unwrap();
    assert!(recover().is_err());
    node.connection
        .execute("DELETE FROM runtime_processes", [])
        .unwrap();
    for (field, value) in [
        ("launch_started", json!(true)),
        ("wrapper_started", json!(true)),
        ("wrapper_pid", json!(123)),
        ("bind_event", json!(id())),
        ("bootstrap", json!({})),
        ("registration", json!({})),
    ] {
        let mut changed = serde_json::to_value(&p).unwrap();
        changed[field] = value;
        node.connection
            .execute(
                "UPDATE jobs SET checkpoint=?2 WHERE id=?1",
                rusqlite::params![a.id, changed.to_string()],
            )
            .unwrap();
        assert!(recover().is_err(), "{field}");
    }
    node.connection
        .execute(
            "UPDATE jobs SET state='cancelling',checkpoint=?2 WHERE id=?1",
            rusqlite::params![a.id, serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
    assert!(recover().is_err());
    node.connection
        .execute("UPDATE jobs SET state='launching'", [])
        .unwrap();
    node.connection
        .execute("UPDATE commands SET record='{}'", [])
        .unwrap();
    assert!(
        recover().is_err(),
        "invalid original signed authority cannot be recovered"
    );
    assert!(plan(&node, &a.id).registration_started);
}

#[test]
fn registration_recovery_node_rolls_back_failed_checkpoint_and_bounds_history() {
    let (_f, node, a, _) = prepared();
    node.connection.execute_batch("CREATE TRIGGER reject_recovery BEFORE UPDATE OF checkpoint ON jobs BEGIN SELECT RAISE(ABORT,'injected checkpoint failure'); END;").unwrap();
    assert!(recover(&node.dir, true, &a.id, a.epoch, "recover").is_err());
    assert!(plan(&node, &a.id).registration_started);
    assert_eq!(
        node.connection
            .query_row("SELECT count(*) FROM registration_recoveries", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    node.connection
        .execute_batch("DROP TRIGGER reject_recovery")
        .unwrap();
    node.connection.execute_batch("CREATE TRIGGER reject_recovery_receipt BEFORE INSERT ON registration_recoveries BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;").unwrap();
    assert!(recover(&node.dir, true, &a.id, a.epoch, "lost-receipt").is_err());
    assert!(
        plan(&node, &a.id).registration_started,
        "receipt failure rolls back authorization too"
    );
    node.connection
        .execute_batch("DROP TRIGGER reject_recovery_receipt")
        .unwrap();
    for i in 0..32 {
        node.connection
            .execute(
                "INSERT INTO registration_recoveries VALUES (?1,?2,?3,'{}')",
                rusqlite::params![a.id, format!("previous-{i}"), a.epoch],
            )
            .unwrap();
    }
    assert!(
        recover(&node.dir, true, &a.id, a.epoch, "recover")
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
}

#[test]
fn registration_recovery_schema_upgrade_preserves_prior_receipts_and_rejects_older_recovery() {
    let (mut f, node, a, _) = prepared();
    let original: String = node
        .connection
        .query_row("SELECT checkpoint FROM jobs WHERE id=?1", [&a.id], |r| {
            r.get(0)
        })
        .unwrap();
    node.connection
        .execute_batch("DROP TABLE registration_recoveries")
        .unwrap();
    node.connection
        .pragma_update(None, "user_version", 7)
        .unwrap();
    f.store
        .connection
        .execute_batch("DROP TABLE registration_recoveries")
        .unwrap();
    f.store
        .connection
        .pragma_update(None, "user_version", 8)
        .unwrap();
    assert!(
        recover(&node.dir, true, &a.id, a.epoch, "too-old")
            .unwrap_err()
            .to_string()
            .contains("schema")
    );
    drop(node);
    let node = node_store(&f);
    f.store = Store::open(&f.store.dir).unwrap();
    assert_eq!(
        node.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        8
    );
    assert_eq!(
        f.store
            .connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        9
    );
    let after: String = node
        .connection
        .query_row("SELECT checkpoint FROM jobs WHERE id=?1", [&a.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(after, original);
    recover(&node.dir, true, &a.id, a.epoch, "now-current").unwrap();
}

#[test]
fn registration_recovery_node_requires_unexpired_original_owner_route_and_content() {
    let (f, node, a, _) = prepared();
    let wire: String = node
        .connection
        .query_row("SELECT record FROM commands", [], |r| r.get(0))
        .unwrap();
    let original: SignedRecord = serde_json::from_str(&wire).unwrap();
    let owner = Identity::load(&f.store.dir.join("owner.key")).unwrap();
    for case in ["expired", "route", "epoch", "body", "retained-hash"] {
        let mut record = original.record.clone();
        match case {
            "expired" => {
                record.created_at = now() - 100;
                record.expires_at = now() - 1;
            }
            "route" => record.recipient = owner.did(),
            "epoch" => record.epoch += 1,
            "body" => record.payload["body"]["profile"]["runtime"] = json!("claude"),
            "retained-hash" => (),
            _ => unreachable!(),
        }
        let changed = owner.sign(record).unwrap();
        node.connection
            .execute(
                "UPDATE commands SET record=?1,content_hash=?2",
                rusqlite::params![
                    serde_json::to_string(&changed).unwrap(),
                    if case == "retained-hash" {
                        "bad-hash"
                    } else {
                        &changed.content_hash
                    }
                ],
            )
            .unwrap();
        assert!(
            recover(&node.dir, true, &a.id, a.epoch, case).is_err(),
            "{case}"
        );
        assert!(plan(&node, &a.id).registration_started);
        assert_eq!(
            node.connection
                .query_row("SELECT count(*) FROM registration_recoveries", [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}
