//! Review and integration decisions are authorized here. Git/check evidence is
//! produced separately by the controller executor, never accepted from a worker.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use super::model::*;
use super::store::{
    active_run, document, effect, issue, memory_snapshot, now, revision, save_attempt, save_task,
    supervise, worker,
};

pub fn integration(c: &Connection, id: &str) -> Result<Integration> {
    let text: String = c
        .query_row("SELECT doc FROM integrations WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .context("unknown integration")?;
    Ok(serde_json::from_str(&text)?)
}

pub(super) fn save(c: &Connection, i: &Integration) -> Result<()> {
    c.execute(
        "UPDATE integrations SET candidate=?2,state=?3,doc=?4 WHERE id=?1",
        params![
            i.id,
            i.candidate_oid.as_deref().unwrap_or(""),
            i.state,
            serde_json::to_string(i)?
        ],
    )?;
    Ok(())
}

pub(super) fn policy_hash(r: &Run, t: &Task) -> Result<String> {
    Ok(digest(&serde_json::to_vec(
        &json!({"policy":r.policy,"contract":t.contract_hash}),
    )?))
}

pub(super) fn current(c: &Connection, i: &Integration) -> Result<(Run, Task)> {
    let r: Run = document(c, "runs", &i.run)?;
    let t: Task = document(c, "tasks", &i.task)?;
    ensure!(
        t.contract_hash == i.contract_hash && policy_hash(&r, &t)? == i.policy_hash,
        "candidate contract or policy changed"
    );
    Ok((r, t))
}

pub(super) fn gates(c: &Connection, i: &Integration) -> Result<()> {
    let (r, t) = current(c, i)?;
    let candidate = i.candidate_oid.as_ref().context("candidate not prepared")?;
    oid(candidate)?;
    let mut receipts = Vec::<Value>::new();
    for name in &t.contract.checks {
        let (state, doc): (String, String) = c
            .query_row(
                "SELECT state,doc FROM check_runs WHERE integration=?1 AND name=?2",
                params![i.id, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context("check receipt missing")?;
        let receipt: Value = serde_json::from_str(&doc)?;
        ensure!(
            state == "passed"
                && receipt["candidate"] == *candidate
                && receipt["policy_hash"] == i.policy_hash,
            "check is not a passed receipt for this candidate/policy"
        );
        receipts.push(receipt);
    }
    ensure!(
        i.checks_digest.as_deref() == Some(&digest(&serde_json::to_vec(&receipts)?)),
        "check receipts changed"
    );
    if r.policy.require_review {
        let author: String = c.query_row(
            "SELECT json_extract(doc,'$.author') FROM results WHERE attempt=?1",
            [&i.attempt],
            |r| r.get(0),
        )?;
        let approved:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM reviews v JOIN review_assignments a ON a.attempt=v.id WHERE a.integration=?1 AND v.task=?2 AND v.candidate=?3 AND v.contract_hash=?4 AND v.principal<>?5 AND json_extract(v.doc,'$.approve')=1 AND json_extract(v.doc,'$.checks_digest')=?6)",params![i.id,i.task,candidate,i.contract_hash,author,i.checks_digest],|r|r.get(0))?;
        ensure!(
            approved,
            "independent review missing for exact candidate and check receipts"
        );
    }
    Ok(())
}

pub(super) fn mutate(tx: &Transaction<'_>, p: &Principal, command: &Mutation) -> Result<Value> {
    match command {
        Mutation::RequestVerification {
            attempt,
            expected_revision,
        } => {
            let a: Attempt = document(tx, "attempts", attempt)?;
            let mut t: Task = document(tx, "tasks", &a.task)?;
            supervise(p, &t.run)?;
            revision(t.revision, *expected_revision)?;
            let r: Run = document(tx, "runs", &t.run)?;
            active_run(&r)?;
            ensure!(
                a.role == "worker" && a.contract_hash == t.contract_hash,
                "result attempt no longer matches task"
            );
            let latest: u64 = tx.query_row(
                "SELECT MAX(number) FROM attempts WHERE task=?1 AND role='worker'",
                [&t.id],
                |r| r.get(0),
            )?;
            ensure!(a.number == latest, "result belongs to a superseded attempt");
            ensure!(
                ["awaiting_review", "changes_requested"].contains(&t.state.as_str()),
                "task is not awaiting verification"
            );
            ensure!(
                t.contract.kind == "code",
                "artifact verification requires an artifact verifier"
            );
            let result_oid: String = tx
                .query_row(
                    "SELECT commit_oid FROM results WHERE attempt=?1 AND contract_hash=?2",
                    params![attempt, t.contract_hash],
                    |r| r.get(0),
                )
                .optional()?
                .context("immutable result missing")?;
            oid(&result_oid)?;
            let i = Integration {
                id: id(),
                run: r.id.clone(),
                task: t.id.clone(),
                attempt: a.id,
                base_oid: r.integration_oid.clone(),
                result_oid,
                candidate_oid: None,
                tree_oid: None,
                contract_hash: t.contract_hash.clone(),
                policy_hash: policy_hash(&r, &t)?,
                checks_digest: None,
                state: "preparing".into(),
                revision: 1,
                created_at: now(),
                error: None,
            };
            tx.execute(
                "INSERT INTO integrations VALUES (?1,?2,?3,?4,'','preparing',?5)",
                params![i.id, i.run, i.task, i.base_oid, serde_json::to_string(&i)?],
            )?;
            t.state = "verifying".into();
            t.revision += 1;
            save_task(tx, &t)?;
            effect(
                tx,
                "controller",
                "verify_candidate",
                &serde_json::to_value(&i)?,
                r.deadline,
            )?;
            Ok(serde_json::to_value(i)?)
        }
        Mutation::RequestReview {
            integration: id,
            expected_revision,
            node,
            profile,
        } => {
            let mut i = integration(tx, id)?;
            supervise(p, &i.run)?;
            revision(i.revision, *expected_revision)?;
            let (r, t) = current(tx, &i)?;
            active_run(&r)?;
            ensure!(
                i.state == "awaiting_review",
                "candidate is not awaiting independent review"
            );
            let prior: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM review_assignments ra JOIN attempts a ON a.id=ra.attempt WHERE ra.integration=?1 AND (a.active=1 OR EXISTS(SELECT 1 FROM reviews r WHERE r.id=a.id)))",
                [id],
                |r| r.get(0),
            )?;
            ensure!(
                !prior,
                "candidate already has a live reviewer or a completed review"
            );
            let prof = r
                .policy
                .profiles
                .get(profile)
                .context("review profile not allowed")?;
            let node_limit: u32 = tx
                .query_row(
                    "SELECT max_active FROM nodes WHERE id=?1 AND active=1",
                    [node],
                    |r| r.get(0),
                )
                .optional()?
                .context("review node not enrolled")?;
            let active: u32 = tx.query_row(
                "SELECT count(*) FROM attempts WHERE run=?1 AND active=1",
                [&r.id],
                |r| r.get(0),
            )?;
            let on_node: u32 = tx.query_row(
                "SELECT count(*) FROM attempts WHERE node=?1 AND active=1",
                [node],
                |r| r.get(0),
            )?;
            let on_profile: u32 = tx.query_row(
                "SELECT count(*) FROM attempts WHERE run=?1 AND profile=?2 AND active=1",
                params![r.id, profile],
                |r| r.get(0),
            )?;
            ensure!(
                active < r.policy.max_active && on_node < node_limit && on_profile < prof.limit,
                "review capacity exhausted"
            );
            let(number,review_count):(u64,u64)=tx.query_row("SELECT COALESCE(MAX(number),0)+1,COALESCE(SUM(role='reviewer'),0) FROM attempts WHERE task=?1",[&t.id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!(
                review_count < (r.policy.max_attempts as u64),
                "review attempt budget exhausted"
            );
            let a = Attempt {
                id: super::model::id(),
                task: t.id.clone(),
                run: r.id.clone(),
                number,
                epoch: 1,
                node: node.clone(),
                profile: profile.clone(),
                role: "reviewer".into(),
                base_oid: i
                    .candidate_oid
                    .clone()
                    .context("review candidate missing")?,
                contract_hash: t.contract_hash.clone(),
                state: "reserved".into(),
                native_session: None,
                resource: None,
            };
            tx.execute(
                "INSERT INTO attempts VALUES (?1,?2,?3,?4,?5,'reviewer',?6,1,'reserved',1,?7)",
                params![
                    a.id,
                    a.task,
                    a.run,
                    node,
                    profile,
                    a.number,
                    serde_json::to_string(&a)?
                ],
            )?;
            tx.execute(
                "INSERT INTO review_assignments VALUES (?1,?2)",
                params![a.id, i.id],
            )?;
            let credential = issue(tx, "reviewer", Some(&a.run), Some(&a.id), None, a.epoch)?;
            let review = ReviewContext {
                integration: i.id.clone(),
                candidate_oid: a.base_oid.clone(),
                base_oid: i.base_oid.clone(),
                result_oid: i.result_oid.clone(),
                checks_digest: i
                    .checks_digest
                    .clone()
                    .context("review needs check receipts")?,
            };
            let memory = memory_snapshot(tx, &r.id, &t.contract.memory_topics, 32000)?;
            effect(
                tx,
                node,
                "execute",
                &json!({"attempt":a,"contract":t.contract,"profile":prof,"repo":r.repo,"credential":credential,"memory":memory,"review":review,"memory_version":super::live_memory::version(tx,&r.id)?}),
                r.deadline,
            )?;
            i.revision += 1;
            save(tx, &i)?;
            Ok(json!({"id":a.id,"attempt":a,"integration":i}))
        }
        Mutation::SubmitReview {
            attempt,
            epoch,
            integration: integration_id,
            candidate_oid,
            contract_hash,
            approve,
            criteria,
            findings,
        } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            worker(p, &a, *epoch)?;
            ensure!(
                a.role == "reviewer" && a.state == "running",
                "review requires the assigned ready reviewer"
            );
            let assigned: String = tx.query_row(
                "SELECT integration FROM review_assignments WHERE attempt=?1",
                [attempt],
                |r| r.get(0),
            )?;
            ensure!(
                &assigned == integration_id,
                "reviewer assigned to another candidate"
            );
            let mut i = integration(tx, integration_id)?;
            let (r, mut t) = current(tx, &i)?;
            active_run(&r)?;
            ensure!(
                i.state == "awaiting_review"
                    && i.candidate_oid.as_deref() == Some(candidate_oid)
                    && i.contract_hash == *contract_hash
                    && a.base_oid == *candidate_oid,
                "review candidate or contract is stale"
            );
            let author: Attempt = document(tx, "attempts", &i.attempt)?;
            ensure!(
                a.id != author.id
                    && a.native_session.is_some()
                    && a.native_session != author.native_session,
                "author runtime cannot satisfy independent review"
            );
            ensure!(
                criteria.len() == t.contract.criteria.len(),
                "review must assess every contract criterion"
            );
            let mut seen = std::collections::BTreeSet::new();
            for c in criteria {
                ensure!(
                    c.criterion < t.contract.criteria.len()
                        && seen.insert(c.criterion)
                        && !c.evidence.trim().is_empty()
                        && c.evidence.len() <= 8192,
                    "invalid or duplicate criterion assessment"
                );
            }
            ensure!(
                !*approve || criteria.iter().all(|c| c.passed),
                "approval has failing criteria"
            );
            ensure!(
                *approve || findings.iter().any(|s| !s.trim().is_empty()),
                "changes requested must include actionable findings"
            );
            ensure!(
                findings.len() <= 100 && findings.iter().all(|s| s.len() <= 8192),
                "review findings too large"
            );
            let receipt = json!({"id":attempt,"integration":integration_id,"candidate":candidate_oid,"contract_hash":contract_hash,"checks_digest":i.checks_digest,"approve":approve,"criteria":criteria,"findings":findings,"reviewer":p.id});
            tx.execute(
                "INSERT INTO reviews VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    attempt,
                    t.id,
                    candidate_oid,
                    p.id,
                    contract_hash,
                    serde_json::to_string(&receipt)?
                ],
            )?;
            a.state = "review_submitted".into();
            save_attempt(tx, &a)?;
            i.state = if *approve {
                "verified"
            } else {
                "changes_requested"
            }
            .into();
            i.revision += 1;
            save(tx, &i)?;
            if *approve {
                gates(tx, &i)?;
            }
            t.state = i.state.clone();
            t.revision += 1;
            save_task(tx, &t)?;
            effect(tx, "controller", "review_submitted", &receipt, r.deadline)?;
            Ok(receipt)
        }
        Mutation::ApplyIntegration {
            integration: id,
            expected_revision,
        } => {
            let mut i = integration(tx, id)?;
            supervise(p, &i.run)?;
            revision(i.revision, *expected_revision)?;
            let (r, _) = current(tx, &i)?;
            active_run(&r)?;
            ensure!(
                i.state == "verified" && r.integration_oid == i.base_oid,
                "candidate must be verified against the current integration base"
            );
            gates(tx, &i)?;
            i.state = "applying".into();
            i.revision += 1;
            save(tx, &i)?;
            effect(
                tx,
                "controller",
                "integrate_candidate",
                &serde_json::to_value(&i)?,
                r.deadline,
            )?;
            Ok(serde_json::to_value(i)?)
        }
        _ => bail!("not a verification operation"),
    }
}
