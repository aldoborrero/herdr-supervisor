use super::*;
use crate::control::{
    api,
    pagination::{self, Collection, PAGE_BYTES},
};
use crate::{
    managed,
    managed_refresh::View,
    popup::{RowKind, Section},
};
use std::collections::BTreeSet;

fn proposal(f: &mut Fixture, topic: &str, text: &str) -> String {
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::ProposeMemory {
            run: f.run.clone(),
            topic: topic.into(),
            text: text.into(),
            source_attempt: None,
            evidence: vec!["test provenance".into()],
        },
    )["id"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn project_memory_pages_keep_full_history_and_current_approval_revisions() {
    let mut f = Fixture::new();
    let text = "é".repeat(16000);
    let mut expected = BTreeSet::new();
    for n in 0..65 {
        expected.insert(proposal(&mut f, &format!("topic{n}"), &text));
    }
    let first = proposal(&mut f, "revised", "original");
    expected.insert(first.clone());
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::ApproveMemory {
            proposal: first.clone(),
            expected_revision: 0,
        },
    );
    call(
        &mut f.store,
        &f.supervisor,
        Mutation::WithdrawMemory {
            run: f.run.clone(),
            topic: "revised".into(),
            expected_revision: 1,
        },
    );
    let replacement = proposal(&mut f, "revised", "replacement");
    expected.insert(replacement.clone());
    let before: u64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap();
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    let mut approved_visible = false;
    let mut replacement_checked = false;
    let mut pages = 0;
    loop {
        let page = api::handle(
            &mut f.store,
            api::ApiRequest::ProjectPage {
                token: f.owner.clone(),
                run: f.run.clone(),
                collection: Collection::MemoryProposals,
                cursor,
            },
        )
        .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() <= 3 * PAGE_BYTES);
        assert!(page.get("tasks").is_none());
        assert!(page.get("attempts").is_none());
        for p in page["memory_proposals"].as_array().unwrap() {
            assert!(seen.insert(p["id"].as_str().unwrap().to_string()));
            assert_eq!(p["evidence"], json!(["test provenance"]));
            if p["topic"].as_str().unwrap().starts_with("topic") {
                assert_eq!(p["text"], text);
            }
        }
        for row in managed::page_view(
            "demo",
            Section::Memory,
            Collection::MemoryProposals,
            View::Current(page.clone()),
        ) {
            if let RowKind::Managed { doc, actions, .. } = row.kind {
                if doc["id"] == first {
                    assert!(actions.is_empty());
                    approved_visible = true;
                }
                if doc["id"] == replacement {
                    assert!(
                        matches!(&actions[0].command, Mutation::ApproveMemory {proposal, expected_revision:2} if proposal == &replacement)
                    );
                    replacement_checked = true;
                }
            }
        }
        cursor = page["_page"]["next_cursor"].as_str().map(str::to_owned);
        pages += 1;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 10);
    }
    assert!(pages >= 3);
    assert_eq!(seen, expected);
    assert!(approved_visible && replacement_checked);
    let page =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::Memory, None).unwrap();
    assert_eq!(page["withdrawn_memory"][0]["revision"], 2);
    assert_eq!(page["withdrawn_memory"][0]["text"], "");
    assert!(page["memory"]["topics"].as_object().unwrap().is_empty());
    let rows = managed::page_view(
        "demo",
        Section::Memory,
        Collection::Memory,
        View::Current(page),
    );
    assert!(
        rows.iter()
            .any(|r| r.text.contains("withdrawn at revision 2"))
    );
    assert!(
        rows.iter()
            .all(|r| !matches!(&r.kind,RowKind::Managed {actions,..} if !actions.is_empty()))
    );
    let after: u64 = f
        .store
        .connection
        .query_row("SELECT count(*) FROM journal", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "reading pages must not approve or acknowledge anything"
    );
}

#[test]
fn project_pages_recheck_credentials_and_cursors_and_never_offer_stale_actions() {
    let mut f = Fixture::new();
    let task = f.task();
    let (attempt, token) = f.dispatch(&task);
    let overview = pagination::overview(&f.store, &f.owner, &f.run).unwrap();
    let cursor = overview["collections"]["attempts"]["cursor"]
        .as_str()
        .unwrap();
    let page = pagination::project_page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::Attempts,
        Some(cursor),
    )
    .unwrap();
    assert_eq!(page["attempts"][0]["id"], attempt.id);
    assert!(page["attempts"][0]["profile_snapshot"].is_object());
    assert!(!page.to_string().contains(&token));
    for collection in [
        Collection::Attempts,
        Collection::PendingNotices,
        Collection::Memory,
        Collection::PendingMemory,
    ] {
        for credential in [&token, &f.node] {
            assert!(
                pagination::project_page(&f.store, credential, &f.run, collection, None).is_err()
            );
        }
    }
    assert!(
        pagination::project_page(
            &f.store,
            &f.supervisor,
            &f.run,
            Collection::Attempts,
            Some(cursor)
        )
        .is_err()
    );
    assert!(
        pagination::project_page(
            &f.store,
            &f.owner,
            &f.run,
            Collection::Notices,
            Some(cursor)
        )
        .is_err()
    );
    assert!(pagination::project_page(&f.store, &f.owner, &f.run, Collection::Tasks, None).is_err());
    let rows = managed::page_view(
        "demo",
        Section::Threads,
        Collection::Attempts,
        View::Stale(page, "offline".into()),
    );
    assert!(rows.iter().any(|r| r.text.contains("read only")));
    assert!(
        rows.iter()
            .all(|r| !matches!(&r.kind,RowKind::Managed {actions,..} if !actions.is_empty()))
    );
    for row in rows {
        if let RowKind::Managed { doc, .. } = row.kind {
            assert_eq!(doc["_page_collection"], "attempts");
            assert_eq!(doc["_view_read_only"], true);
        }
    }
}

#[test]
fn project_page_start_cursor_keeps_scan_boundary_until_explicit_restart() {
    let mut f = Fixture::new();
    let original = proposal(&mut f, "parser", "original");
    let first =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::PendingMemory, None)
            .unwrap();
    let cursor = first["_page"]["cursor"].as_str().unwrap();
    let appended = proposal(&mut f, "parser", "added later");
    let refresh = pagination::project_page(
        &f.store,
        &f.owner,
        &f.run,
        Collection::PendingMemory,
        Some(cursor),
    )
    .unwrap();
    assert_eq!(refresh["memory_proposals"].as_array().unwrap().len(), 1);
    assert_eq!(refresh["memory_proposals"][0]["id"], original);
    assert_eq!(refresh["collections"]["pending_memory"]["count"], 2);
    assert!(refresh["_page"]["next_cursor"].is_null());
    let restarted =
        pagination::project_page(&f.store, &f.owner, &f.run, Collection::PendingMemory, None)
            .unwrap();
    assert_eq!(restarted["memory_proposals"].as_array().unwrap().len(), 2);
    assert_eq!(restarted["memory_proposals"][1]["id"], appended);
    for state in [View::Loading, View::Failed("offline".into())] {
        let rows = managed::page_view("demo", Section::Memory, Collection::PendingMemory, state);
        assert!(rows.iter().any(|r|matches!(&r.kind,RowKind::Managed {doc,actions,..} if doc["_view_read_only"]==true && doc["_page_collection"]=="pending_memory" && actions.is_empty())));
    }
}
