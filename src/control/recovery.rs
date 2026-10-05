//! Release cancelled runs only after durable evidence proves their effects idle.
//! Never delete checkouts, reset refs, signal stored PIDs or invent exit receipts.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

use super::{
    model::{Integration, Run},
    store::{Store, document, now, save_run},
};

pub(super) fn blockers(c: &Connection, run: &str) -> Result<Vec<&'static str>> {
    let tests = [
        (
            "worker_exit_unresolved",
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND active=1)",
        ),
        (
            "coordinator_exit_unresolved",
            "SELECT EXISTS(SELECT 1 FROM coordinator_launches WHERE run=?1 AND state<>'exited')",
        ),
        (
            "repository_creation_unresolved",
            "SELECT EXISTS(SELECT 1 FROM repository_bindings WHERE run=?1 AND state<>'ready')",
        ),
        (
            "candidate_preparation_unresolved",
            "SELECT EXISTS(SELECT 1 FROM integrations i WHERE i.run=?1 AND i.state='preparing' AND EXISTS(SELECT 1 FROM repository_bindings b WHERE b.run=i.run))",
        ),
        (
            "git_apply_unresolved",
            "SELECT EXISTS(SELECT 1 FROM integrations WHERE run=?1 AND state='applying')",
        ),
        (
            "check_exit_unresolved",
            "SELECT EXISTS(SELECT 1 FROM check_runs c JOIN integrations i ON i.id=c.integration WHERE i.run=?1 AND c.state='started')",
        ),
        (
            "checkout_creation_unresolved",
            "SELECT EXISTS(SELECT 1 FROM verification_worktrees w JOIN integrations i ON i.id=w.integration WHERE i.run=?1 AND w.state<>'ready')",
        ),
    ];
    let mut blockers = Vec::new();
    for (reason, sql) in tests {
        if c.query_row(sql, [run], |r| r.get::<_, bool>(0))? {
            blockers.push(reason);
        }
    }
    Ok(blockers)
}

fn pending(c: &Connection, run: &str) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM repository_bindings WHERE run=?1 AND active=1) OR EXISTS(SELECT 1 FROM integrations WHERE run=?1 AND state IN ('preparing','checking','awaiting_review','verified','applying'))",[run],|r|r.get(0))?)
}

pub(super) fn status(c: &Connection, run: &Run) -> Result<Value> {
    let binding: Option<bool> = c
        .query_row(
            "SELECT active FROM repository_bindings WHERE run=?1",
            [&run.id],
            |r| r.get(0),
        )
        .optional()?;
    let reasons = if ["cancelled", "cancelling"].contains(&run.state.as_str()) {
        blockers(c, &run.id)?
    } else {
        Vec::new()
    };
    let state = if !["cancelled", "cancelling"].contains(&run.state.as_str()) {
        "not_cancelled"
    } else if run.state == "cancelling" || !reasons.is_empty() {
        "blocked"
    } else if pending(c, &run.id)? {
        "awaiting_reconciliation"
    } else {
        "released"
    };
    Ok(json!({"state":state,"binding_active":binding,"blockers":reasons,"resources_deleted":false}))
}

pub(super) fn blocks_repository(c: &Connection, repo: &str) -> Result<bool> {
    let mut q = c.prepare("SELECT id FROM runs WHERE repo=?1 AND state='cancelled'")?;
    for run in q.query_map([repo], |r| r.get::<_, String>(0))? {
        let run = run?;
        if pending(c, &run)? || !blockers(c, &run)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn cancelled(store: &mut Store) -> Result<Value> {
    // This lease excludes even an executor that has read an active run but has
    // not yet persisted its first repository intent. A crash releases the lease,
    // but started/ambiguous effect records below still prevent reuse.
    let _lease = super::api::lock(&store.dir, "git-executor.lock")?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cursor: i64 = tx
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='cancel-recovery-cursor'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let runs: Vec<(i64, String)> = {
        let mut q=tx.prepare("SELECT r.rowid,r.id FROM runs r WHERE r.state='cancelled' AND (EXISTS(SELECT 1 FROM repository_bindings b WHERE b.run=r.id AND b.active=1) OR EXISTS(SELECT 1 FROM integrations i WHERE i.run=r.id AND i.state IN ('preparing','checking','awaiting_review','verified','applying'))) ORDER BY (r.rowid<=?1),r.rowid LIMIT 64")?;
        q.query_map([cursor], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut released = Vec::new();
    let mut unresolved = Vec::new();
    for (rowid, id) in runs {
        tx.execute("INSERT INTO meta VALUES ('cancel-recovery-cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[rowid.to_string()])?;
        let reasons = blockers(&tx, &id)?;
        if !reasons.is_empty() {
            unresolved.push(json!({"run":id,"blockers":reasons}));
            continue;
        }
        let mut run: Run = document(&tx, "runs", &id)?;
        let candidates: Vec<String> = {
            let mut q=tx.prepare("SELECT doc FROM integrations WHERE run=?1 AND state IN ('preparing','checking','awaiting_review','verified')")?;
            q.query_map([&id], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?
        };
        for doc in candidates {
            let mut i: Integration = serde_json::from_str(&doc)?;
            i.state = "superseded".into();
            i.revision += 1;
            i.error=Some("Run cancelled; unfinished verification retired after all recorded effects became idle".into());
            super::verification::save(&tx, &i)?;
        }
        tx.execute("UPDATE outbox SET state='cancelled' WHERE target='controller' AND kind IN ('verify_candidate','integrate_candidate') AND json_extract(payload,'$.run')=?1 AND state<>'applied'",[&id])?;
        tx.execute(
            "UPDATE repository_bindings SET active=0 WHERE run=?1",
            [&id],
        )?;
        run.revision += 1;
        save_run(&tx, &run)?;
        let result =
            json!({"run":id,"revision":run.revision,"state":"released","resources_deleted":false});
        tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('controller-reconciler',?1,?2,'cancelled_run_reconciled',?3,?4)",params![format!("release-{}-{}",id,run.revision),id,serde_json::to_string(&result)?,now()])?;
        released.push(result);
    }
    tx.commit()?;
    Ok(json!({"released":released,"unresolved":unresolved}))
}
