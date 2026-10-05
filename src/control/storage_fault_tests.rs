//! Exercise SQLITE_FULL from the real pager, not a RAISE(ABORT) approximation.
//! The per-database page limit bounds disk use; no host filesystem is filled.
use super::*;
use rusqlite::{Connection, ErrorCode, types::Value as SqlValue};

pub(super) fn rows(c: &Connection) -> BTreeMap<String, Vec<Vec<SqlValue>>> {
    let names = c
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'pager_full_probe' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut q = c
                .prepare(&format!(
                    "SELECT * FROM \"{}\" ORDER BY rowid",
                    name.replace('"', "\"\"")
                ))
                .unwrap();
            let columns = q.column_count();
            let values = q
                .query_map([], |r| (0..columns).map(|i| r.get(i)).collect())
                .unwrap()
                .collect::<rusqlite::Result<Vec<Vec<SqlValue>>>>()
                .unwrap();
            (name, values)
        })
        .collect()
}

pub(super) fn limit_pages_at(c: &Connection, table: &str) -> i64 {
    assert!(["outbox", "journal", "jobs"].contains(&table));
    let original: i64 = c
        .pragma_query_value(None, "max_page_count", |r| r.get(0))
        .unwrap();
    c.execute_batch("CREATE TABLE pager_full_probe (bytes BLOB) STRICT;")
        .unwrap();
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let page_size: i64 = c
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    let allocation = (pages + 256) * page_size;
    assert!(allocation <= 16 * 1024 * 1024);
    // Plenty of room for the small ordinary mutation, but not the probe. This
    // obtains SQLITE_FULL after the selected application INSERT has happened.
    c.execute_batch(&format!("CREATE TRIGGER pager_full AFTER INSERT ON {table} BEGIN INSERT INTO pager_full_probe VALUES(zeroblob({allocation})); END; PRAGMA max_page_count={};", pages + 128)).unwrap();
    original
}

pub(super) fn restore_pages(c: &Connection, original: i64) {
    c.pragma_update(None, "max_page_count", original).unwrap();
    c.execute_batch("DROP TRIGGER pager_full; DROP TABLE pager_full_probe;")
        .unwrap();
}

pub(super) fn assert_disk_full(error: anyhow::Error) {
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(|e| e.sqlite_error_code()),
        Some(ErrorCode::DiskFull),
        "expected a real SQLite pager failure: {error:#}"
    );
}

#[test]
fn sqlite_full_rolls_back_dispatch_and_audit_then_same_operation_recovers_after_restart() {
    for table in ["outbox", "journal"] {
        let mut f = Fixture::new();
        let task = f.task();
        let req = request(
            &f.supervisor,
            Mutation::Dispatch {
                task,
                expected_revision: 1,
                node: "local".into(),
                profile: "auto".into(),
            },
        );
        let before = rows(&f.store.connection);
        let original = limit_pages_at(&f.store.connection, table);
        assert_disk_full(f.store.apply(&req).unwrap_err());
        // SQLITE_FULL can abort the entire transaction itself, unlike ABORT.
        assert!(f.store.connection.is_autocommit());
        assert_eq!(rows(&f.store.connection), before, "partial {table} effects");
        restore_pages(&f.store.connection, original);
        drop(f.store);
        f.store = Store::open(&f.dir.path().join("state")).unwrap();
        assert_eq!(rows(&f.store.connection), before);
        let receipt = f.store.apply(&req).unwrap();
        assert_eq!(receipt, f.store.apply(&req).unwrap());
        assert_eq!(f.store.pending(&f.node, "local").unwrap().len(), 1);
        assert_eq!(
            f.store
                .connection
                .query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            f.store
                .connection
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
}
