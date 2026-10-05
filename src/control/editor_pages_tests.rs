use super::*;
use crate::control::{
    api,
    pagination::{self, Collection},
};
use crate::{
    managed, managed_profile_form,
    managed_refresh::View,
    managed_task_form,
    popup::{RowKind, Section},
};

fn submit(f: &mut Fixture, submission: &crate::managed_form::Submission) -> anyhow::Result<Value> {
    f.store.apply_in_run(
        &f.run,
        &Request {
            token: f.owner.clone(),
            operation_id: submission.args[4].clone(),
            command: serde_json::from_str(&submission.request)?,
        },
    )
}
fn current_run(f: &Fixture) -> Run {
    document(&f.store.connection, "runs", &f.run).unwrap()
}

#[test]
fn task_page_edit_preserves_off_page_dependencies_and_new_tasks_validate_them_authoritatively() {
    let mut f = Fixture::new();
    let ids: Vec<_> = (0..61).map(|_| f.task()).collect();
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::AddDependency {
            task: ids[0].clone(),
            expected_revision: 1,
            predecessor: ids[60].clone(),
        },
    );
    let page =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Tasks, None).unwrap();
    assert_eq!(page["tasks"].as_array().unwrap().len(), 50);
    assert!(page["run"]["policy"].get("profiles").is_none());
    assert_eq!(page["tasks"][0]["dependencies"], json!([ids[60]]));
    assert!(
        !page["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == ids[60])
    );
    assert_eq!(page["checks_allowed"], json!(["test"]));
    let mut edit = managed_task_form::Form::edit(&current_run(&f).project, &page, &ids[0]).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&edit.fields[6].value).unwrap(),
        json!([ids[60]])
    );
    edit.fields[1].value = "Updated through a partial task index".into();
    let request = edit.prepare().unwrap();
    let changed = submit(&mut f, &request).unwrap();
    assert_eq!(changed["revision"], 3);
    assert_eq!(submit(&mut f, &request).unwrap(), changed);
    let refreshed =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Tasks, None).unwrap();
    assert_eq!(refreshed["tasks"][0]["dependencies"], json!([ids[60]]));
    let rows = managed::page_view(
        &current_run(&f).project,
        Section::Tasks,
        Collection::Tasks,
        View::Current(refreshed.clone()),
    );
    let captured = rows
        .iter()
        .find_map(|r| match &r.kind {
            RowKind::Managed { doc, .. } if !doc["_task_form"].is_null() => {
                Some(&doc["_task_form"])
            }
            _ => None,
        })
        .unwrap();
    let mut create = managed_task_form::Form::new(&current_run(&f).project, captured).unwrap();
    create.fields[0].value = "New task".into();
    create.fields[1].value = "Follow the existing task".into();
    create.fields[2].value = r#"["done"]"#.into();
    create.fields[6].value = serde_json::to_string(&[&ids[60]]).unwrap();
    let created = submit(&mut f, &create.prepare().unwrap()).unwrap();
    let count: u64 = f
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM dependencies WHERE task=?1 AND predecessor=?2",
            rusqlite::params![created["id"].as_str().unwrap(), ids[60]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let page =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Tasks, None).unwrap();
    let mut invalid = managed_task_form::Form::new(&current_run(&f).project, &page).unwrap();
    invalid.fields[0].value = "Invalid dependency".into();
    invalid.fields[1].value = "Must roll back".into();
    invalid.fields[2].value = r#"["done"]"#.into();
    invalid.fields[6].value = r#"["absent-task"]"#.into();
    let before = current_run(&f).revision;
    assert!(submit(&mut f, &invalid.prepare().unwrap()).is_err());
    assert_eq!(current_run(&f).revision, before);
    assert_eq!(
        pagination::overview(&f.store, &f.owner, &f.run).unwrap()["collections"]["tasks"]["count"],
        62
    );
    let mut edit = managed_task_form::Form::edit(&current_run(&f).project, &page, &ids[0]).unwrap();
    edit.fields[6].value = serde_json::to_string(&[&ids[0]]).unwrap();
    assert!(edit.prepare().is_err());
}

#[test]
fn profile_page_creation_cannot_replace_a_hidden_name_and_edits_capture_page_revision() {
    let mut f = Fixture::new();
    for n in 0..50 {
        let revision = current_run(&f).revision;
        call(
            &mut f.store,
            &f.owner,
            Mutation::CreateProfile {
                run: f.run.clone(),
                expected_revision: revision,
                name: format!("p{n:03}"),
                profile: policy().profiles["auto"].clone(),
            },
        );
    }
    let first =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Profiles, None).unwrap();
    assert_eq!(first["profiles"].as_object().unwrap().len(), 50);
    assert!(first["profiles"].get("p049").is_none());
    let project = current_run(&f).project;
    let mut hidden = managed_profile_form::Form::new(&project, &first, None).unwrap();
    hidden.fields[0].value = "p049".into();
    hidden.fields[1].value = "pi".into();
    let request = hidden.prepare().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&request.request).unwrap()["method"],
        "create_profile"
    );
    let before = current_run(&f);
    assert!(
        submit(&mut f, &request)
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
    assert_eq!(current_run(&f).policy, before.policy);
    assert_eq!(current_run(&f).revision, before.revision);
    let page = pagination::project_page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::Profiles,
        first["_page"]["next_cursor"].as_str(),
    )
    .unwrap();
    assert_eq!(page["profiles"].as_object().unwrap().len(), 1);
    let rows = managed::page_view(
        &project,
        Section::Settings,
        Collection::Profiles,
        View::Current(page.clone()),
    );
    assert!(rows.iter().any(|r|matches!(&r.kind,RowKind::Managed {doc,actions,..} if doc["_profile_name"]=="p049" && actions.iter().any(|a|a.key=='d'))),"the final page is not the entire profile set");
    let mut edit = managed_profile_form::Form::new(&project, &page, Some("p049")).unwrap();
    edit.fields[1].value = "pi".into();
    let request = edit.prepare().unwrap();
    submit(&mut f, &request).unwrap();
    assert_eq!(current_run(&f).policy.profiles["p049"].runtime, "pi");
    assert!(
        pagination::project_page(
            &f.store,
            &f.owner,
            &f.run,
            Collection::Profiles,
            first["_page"]["next_cursor"].as_str()
        )
        .is_err(),
        "a policy cursor cannot silently rebase after editing"
    );
    let fresh =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Profiles, None).unwrap();
    let mut create = managed_profile_form::Form::new(&project, &fresh, None).unwrap();
    create.fields[0].value = "new-profile".into();
    let submission = create.prepare().unwrap();
    let receipt = submit(&mut f, &submission).unwrap();
    assert_eq!(submit(&mut f, &submission).unwrap(), receipt);
    assert!(receipt.get("policy").is_none());
    let mut stale = managed_profile_form::Form::new(&project, &fresh, None).unwrap();
    stale.fields[0].value = "stale-profile".into();
    assert!(submit(&mut f, &stale.prepare().unwrap()).is_err());
    assert!(
        !current_run(&f)
            .policy
            .profiles
            .contains_key("stale-profile")
    );
}

#[test]
fn create_profile_is_owner_scoped_and_rolls_back_with_its_audit_and_operation() {
    let mut f = Fixture::new();
    let revision = current_run(&f).revision;
    let request = request(
        &f.owner,
        Mutation::CreateProfile {
            run: f.run.clone(),
            expected_revision: revision,
            name: "fresh".into(),
            profile: policy().profiles["auto"].clone(),
        },
    );
    let denied = Request {
        token: f.supervisor.clone(),
        operation_id: request.operation_id.clone(),
        command: request.command.clone(),
    };
    assert!(f.store.apply_in_run(&f.run, &denied).is_err());
    assert!(f.store.apply_in_run("foreign-run", &request).is_err());
    f.store.connection.execute_batch("CREATE TRIGGER fail_profile_creation BEFORE INSERT ON journal WHEN NEW.kind='create_profile' BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;").unwrap();
    assert!(f.store.apply_in_run(&f.run, &request).is_err());
    assert_eq!(current_run(&f).revision, revision);
    assert!(!current_run(&f).policy.profiles.contains_key("fresh"));
    let count: u64 = f
        .store
        .connection
        .query_row(
            "SELECT count(*) FROM operations WHERE id=?1",
            [&request.operation_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    f.store
        .connection
        .execute_batch("DROP TRIGGER fail_profile_creation;")
        .unwrap();
    let receipt = api::handle(
        &mut f.store,
        api::ApiRequest::ProjectApply {
            token: request.token.clone(),
            run: f.run.clone(),
            operation_id: request.operation_id.clone(),
            command: Box::new(request.command.clone()),
        },
    )
    .unwrap();
    assert_eq!(receipt["revision"], revision + 1);
    assert_eq!(f.store.apply_in_run(&f.run, &request).unwrap(), receipt);
}
