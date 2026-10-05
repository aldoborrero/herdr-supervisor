use super::*;
use crate::control::{model::Profile, store::Store};
use std::collections::BTreeMap;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    spec: Spec,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("controller");
        Store::initialize(&state).unwrap();
        let root = dir.path().join("projects");
        let spec = Spec {
            name: "Managed example".into(),
            controller: state.clone(),
            credential: state.join("admin.token"),
            repo: "/controller/repo".into(),
            base_oid: "a".repeat(40),
            objective: "Implement a parser".into(),
            policy: Policy {
                max_active: 2,
                reserved_reviewers: 1,
                max_attempts: 3,
                duration_secs: 3600,
                require_review: true,
                profiles: BTreeMap::from([(
                    "auto".into(),
                    Profile {
                        runtime: "codex".into(),
                        argv: vec![],
                        limit: 2,
                    },
                )]),
                checks: BTreeMap::from([("test".into(), vec!["cargo".into(), "test".into()])]),
            },
            launch_config: None,
        };
        Self {
            _dir: dir,
            root,
            spec,
        }
    }
    fn apply(path: &Path, request: &ApiRequest) -> Result<Value> {
        api::handle(
            &mut Store::open(path)?,
            serde_json::from_value(serde_json::to_value(request)?)?,
        )
    }
    fn create(&self) -> Result<Value> {
        create_with(&self.root, self.spec.clone(), Self::apply, |_, _, _| Ok(()))
    }
    fn intent_path(&self) -> PathBuf {
        self.root.join(".managed-setup/managed-example/intent.json")
    }
    fn intent(&self) -> Intent {
        serde_json::from_slice(&std::fs::read(self.intent_path()).unwrap()).unwrap()
    }
    fn count(&self, table: &str) -> u64 {
        Store::open(&self.spec.controller)
            .unwrap()
            .connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
}

#[test]
fn managed_setup_is_visible_only_after_binding_and_replays_one_run_without_launching_models() {
    let f = Fixture::new();
    let first = create_with(
        &f.root,
        f.spec.clone(),
        |path, req| {
            assert!(project::list_slugs(&f.root).is_empty());
            assert!(
                !f.intent().operation.is_empty(),
                "intent must precede controller mutation"
            );
            Fixture::apply(path, req)
        },
        |phase, _, published_path| {
            if phase == Publication::Before {
                assert!(project::list_slugs(&f.root).is_empty());
            } else {
                assert_eq!(project::list_slugs(&f.root), vec!["managed-example"]);
                let project = Project::load_view(&f.root, "managed-example")?;
                assert!(managed::present(&project));
                assert!(managed::require_legacy(&project).is_err());
                assert!(published_path.join("AGENTS.md").is_file());
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(f.create().unwrap(), first);
    assert_eq!(f.count("runs"), 1);
    assert_eq!(f.count("attempts"), 0);
    assert_eq!(f.count("coordinator_launches"), 0);
    assert_eq!(first["models_started"], false);
    assert!(f.intent().published);
    let token = std::fs::read_to_string(&f.spec.credential).unwrap();
    assert!(!first.to_string().contains(&token));
    assert!(
        !std::fs::read_to_string(f.intent_path())
            .unwrap()
            .contains(&token)
    );
    let project = Project::load_view(&f.root, "managed-example").unwrap();
    assert_eq!(
        managed::binding(&project).unwrap().run,
        first["run"].as_str().unwrap()
    );
}

#[test]
fn managed_setup_lost_controller_response_reuses_frozen_operation_after_restart() {
    let f = Fixture::new();
    let first = create_with(
        &f.root,
        f.spec.clone(),
        |path, req| {
            Fixture::apply(path, req)?;
            anyhow::bail!("controller response lost")
        },
        |_, _, _| Ok(()),
    );
    assert!(first.is_err());
    assert_eq!(f.count("runs"), 1);
    assert!(project::list_slugs(&f.root).is_empty());
    let before = f.intent();
    assert!(before.run.is_none());
    let status = status(&f.root, "managed-example").unwrap();
    assert_eq!(status["recorded_state"], "controller_unconfirmed");
    assert_eq!(status["current_controller_state_verified"], false);
    let done = f.create().unwrap();
    assert_eq!(done["operation"], before.operation);
    assert_eq!(f.count("runs"), 1);
    assert_eq!(f.count("operations"), 1);
}

#[test]
fn managed_setup_lost_publication_receipt_recovers_without_overwriting_the_published_view() {
    let f = Fixture::new();
    let first = create_with(&f.root, f.spec.clone(), Fixture::apply, |phase, _, _| {
        if phase == Publication::After {
            anyhow::bail!("receipt write failed");
        }
        Ok(())
    });
    assert!(first.is_err());
    assert!(!f.intent().published);
    assert!(f.intent().run.is_some());
    assert_eq!(
        status(&f.root, "managed-example").unwrap()["recorded_state"],
        "publication_unconfirmed"
    );
    let path = f.root.join("managed-example/PROJECT.md");
    let original =
        std::fs::read_to_string(&path).unwrap() + "\nOperator notes after publication.\n";
    std::fs::write(&path, &original).unwrap();
    let result = f.create().unwrap();
    assert_eq!(f.count("runs"), 1);
    assert_eq!(result["state"], "managed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn managed_setup_keeps_scope_and_credentials_frozen_after_a_lost_response() {
    let f = Fixture::new();
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| anyhow::bail!("offline"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    let mut changed = f.spec.clone();
    changed.objective = "A different objective".into();
    assert!(
        create_with(
            &f.root,
            changed,
            |_, _| panic!("changed request reached controller"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    std::fs::write(
        &f.spec.credential,
        "different-owner-token-00000000000000000000000000",
    )
    .unwrap();
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| panic!("changed credential reached controller"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(f.count("runs"), 0);
    assert!(project::list_slugs(&f.root).is_empty());
}

#[test]
fn managed_setup_refuses_foreign_destinations_and_atomic_publish_cannot_replace_even_empty_directories()
 {
    for race in [false, true] {
        let f = Fixture::new();
        let destination = f.root.join("managed-example");
        if !race {
            std::fs::create_dir_all(&destination).unwrap();
        }
        let outcome = create_with(
            &f.root,
            f.spec.clone(),
            |path, req| {
                assert!(race, "preexisting destination reached controller");
                Fixture::apply(path, req)
            },
            |phase, _, dest| {
                if race && phase == Publication::Before {
                    std::fs::create_dir(dest)?;
                }
                Ok(())
            },
        );
        assert!(outcome.is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 0);
        assert_eq!(f.count("runs"), u64::from(race));
        assert!(project::list_slugs(&f.root).is_empty());
    }
}

#[test]
fn managed_setup_rejects_wrong_receipts_and_never_recreates_a_deleted_published_project() {
    let f = Fixture::new();
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |path, req| {
                let mut result = Fixture::apply(path, req)?;
                result["repo"] = json!("/another/repo");
                Ok(result)
            },
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert!(project::list_slugs(&f.root).is_empty());
    f.create().unwrap();
    std::fs::rename(
        f.root.join("managed-example"),
        f.root.join("moved-by-operator"),
    )
    .unwrap();
    assert!(f.create().is_err());
    assert!(!f.root.join("managed-example").exists());
    assert_eq!(f.count("runs"), 1);
}

#[test]
fn managed_setup_private_intent_fences_contention_and_invalid_specs_make_no_project() {
    let f = Fixture::new();
    let mut bad = f.spec.clone();
    bad.policy.max_active = 0;
    assert!(
        create_with(
            &f.root,
            bad,
            |_, _| panic!("invalid policy reached authority"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert!(!f.root.exists());
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| anyhow::bail!("offline"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    let _lease = api::lock(f.intent_path().parent().unwrap(), "setup.lock").unwrap();
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| panic!("concurrent setup reached authority"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(f.count("runs"), 0);
}

#[test]
fn managed_setup_allows_linked_declarative_config_but_refuses_linked_mutable_intent() {
    let f = Fixture::new();
    let config = f._dir.path().join("config.json");
    let link = f._dir.path().join("config-link.json");
    project::write_json(&config, &f.spec).unwrap();
    std::os::unix::fs::symlink(&config, &link).unwrap();
    assert_eq!(read_spec(&link).unwrap(), f.spec);
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| anyhow::bail!("offline"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    let saved = f.intent_path().with_file_name("original.json");
    std::fs::rename(f.intent_path(), &saved).unwrap();
    std::os::unix::fs::symlink(&saved, f.intent_path()).unwrap();
    assert!(
        create_with(
            &f.root,
            f.spec.clone(),
            |_, _| panic!("linked intent reached authority"),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(f.count("runs"), 0);
}
