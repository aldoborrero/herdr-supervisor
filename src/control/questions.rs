//! Scoped task conversations. Text is context, never a contract amendment or
//! integration permission. Resolutions and their delivery intents commit together.
use super::{
    model::{Attempt, Mutation, Principal, Run, id},
    node::ExecuteOrder,
    store::{self, document, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CONTROLLER_SCHEMA: &str = "
CREATE TABLE task_questions (
 id TEXT PRIMARY KEY, run TEXT NOT NULL REFERENCES runs(id),
 attempt TEXT NOT NULL REFERENCES attempts(id), epoch INTEGER NOT NULL,
 revision INTEGER NOT NULL CHECK(revision IN (1,2)), doc TEXT NOT NULL
) STRICT;
CREATE INDEX task_questions_run ON task_questions(run);
PRAGMA user_version=15;";

pub const NODE_SCHEMA: &str = "
CREATE TABLE worker_questions (
 id TEXT PRIMARY KEY, attempt TEXT NOT NULL REFERENCES jobs(id),
 epoch INTEGER NOT NULL, revision INTEGER NOT NULL CHECK(revision IN (1,2)), doc TEXT NOT NULL
) STRICT;
CREATE TABLE question_notices (
 question TEXT PRIMARY KEY REFERENCES worker_questions(id),
 attempt TEXT NOT NULL REFERENCES jobs(id), epoch INTEGER NOT NULL,
 state TEXT NOT NULL, prepared TEXT, token_hash TEXT, receipt TEXT,
 tries INTEGER NOT NULL DEFAULT 0
) STRICT;
PRAGMA user_version=12;";

const MAX_QUESTIONS: i64 = 32;
fn text(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= 4096 && !value.contains('\0'),
        "question/answer must contain 1..4096 UTF-8 bytes without NUL"
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Question {
    pub id: String,
    pub run: String,
    pub task: String,
    pub attempt: String,
    pub epoch: u64,
    pub contract_hash: String,
    pub text: String,
    pub revision: u64,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    pub answered_generation: Option<u64>,
    pub created_at: i64,
    pub answered_at: Option<i64>,
}

fn run(c: &Connection, a: &Attempt) -> Result<Run> {
    ensure!(
        !["stopped", "cancelling", "retired"].contains(&a.state.as_str()),
        "question attempt is retired"
    );
    let r: Run = document(c, "runs", &a.run)?;
    ensure!(
        ["active", "paused"].contains(&r.state.as_str()) && r.deadline > now(),
        "question run is closed or expired"
    );
    Ok(r)
}

pub(super) fn mutate(tx: &Transaction<'_>, p: &Principal, command: &Mutation) -> Result<Value> {
    let q = match command {
        Mutation::AskQuestion {
            attempt,
            epoch,
            text: body,
        } => {
            text(body)?;
            let a: Attempt = document(tx, "attempts", attempt)?;
            store::worker(p, &a, *epoch)?;
            run(tx, &a)?;
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM task_questions WHERE attempt=?1",
                [attempt],
                |r| r.get(0),
            )?;
            ensure!(count < MAX_QUESTIONS, "attempt question limit reached");
            let q = Question {
                id: id(),
                run: a.run,
                task: a.task,
                attempt: a.id,
                epoch: *epoch,
                contract_hash: a.contract_hash,
                text: body.clone(),
                revision: 1,
                answer: None,
                answered_by: None,
                answered_generation: None,
                created_at: now(),
                answered_at: None,
            };
            tx.execute(
                "INSERT INTO task_questions VALUES (?1,?2,?3,?4,1,?5)",
                params![q.id, q.run, q.attempt, q.epoch, serde_json::to_string(&q)?],
            )?;
            q
        }
        Mutation::AnswerQuestion {
            question,
            expected_revision,
            answer,
        } => {
            text(answer)?;
            let raw: String = tx.query_row(
                "SELECT doc FROM task_questions WHERE id=?1",
                [question],
                |r| r.get(0),
            )?;
            let mut q: Question = serde_json::from_str(&raw)?;
            store::supervise(p, &q.run)?;
            store::revision(q.revision, *expected_revision)?;
            ensure!(
                q.answer.is_none() && q.revision == 1,
                "question already resolved"
            );
            let a: Attempt = document(tx, "attempts", &q.attempt)?;
            ensure!(
                a.epoch == q.epoch && a.contract_hash == q.contract_hash,
                "question assignment changed"
            );
            run(tx, &a)?;
            q.revision = 2;
            q.answer = Some(answer.clone());
            q.answered_by = Some(p.id.clone());
            q.answered_generation = Some(p.generation);
            q.answered_at = Some(now());
            ensure!(
                tx.execute(
                    "UPDATE task_questions SET revision=2,doc=?2 WHERE id=?1 AND revision=1",
                    params![q.id, serde_json::to_string(&q)?]
                )? == 1,
                "question changed"
            );
            q
        }
        _ => anyhow::bail!("unsupported question operation"),
    };
    let a: Attempt = document(tx, "attempts", &q.attempt)?;
    let r = run(tx, &a)?;
    store::effect(
        tx,
        &a.node,
        "question_update",
        &serde_json::to_value(&q)?,
        r.deadline,
    )?;
    // Journal aggregate remains the attempt so coordinator routing finds its run.
    Ok(json!({"id":q.attempt,"question":q}))
}

fn validate(q: &Question, order: &ExecuteOrder) -> Result<()> {
    uuid::Uuid::parse_str(&q.id)?;
    text(&q.text)?;
    ensure!(
        q.run == order.attempt.run
            && q.task == order.attempt.task
            && q.attempt == order.attempt.id
            && q.epoch == order.attempt.epoch
            && q.contract_hash == order.attempt.contract_hash,
        "question outside assignment"
    );
    ensure!(q.created_at > 0, "invalid question timestamp");
    if q.revision == 1 {
        ensure!(
            q.answer.is_none()
                && q.answered_by.is_none()
                && q.answered_generation.is_none()
                && q.answered_at.is_none(),
            "unresolved question has answer fields"
        );
    } else {
        ensure!(
            q.revision == 2
                && q.answered_by.as_ref().is_some_and(|s| !s.is_empty())
                && q.answered_generation.is_some_and(|v| v > 0)
                && q.answered_at.is_some_and(|v| v >= q.created_at),
            "invalid question resolution"
        );
        text(q.answer.as_deref().context("answer missing")?)?;
    }
    Ok(())
}

pub(super) fn receive(tx: &Transaction<'_>, body: &Value) -> Result<()> {
    let q: Question = serde_json::from_value(body.clone())?;
    let (epoch, state, raw): (u64, String, String) = tx.query_row(
        "SELECT epoch,state,doc FROM jobs WHERE id=?1",
        [&q.attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(q.epoch <= epoch, "question epoch is ahead of assignment");
    if q.epoch < epoch || ["stopped", "cancelling"].contains(&state.as_str()) {
        return Ok(());
    }
    let order: ExecuteOrder = serde_json::from_str(&raw)?;
    validate(&q, &order)?;
    let old: Option<String> = tx
        .query_row(
            "SELECT doc FROM worker_questions WHERE id=?1",
            [&q.id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(raw) = old {
        let old: Question = serde_json::from_str(&raw)?;
        let mut original = old.clone();
        original.answer = q.answer.clone();
        original.answered_by = q.answered_by.clone();
        original.answered_generation = q.answered_generation;
        original.answered_at = q.answered_at;
        original.revision = q.revision;
        ensure!(original == q, "question identity/content collision");
        if q.revision <= old.revision {
            ensure!(
                q.revision != old.revision || old == q,
                "question resolution collision"
            );
            return Ok(());
        }
    } else {
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM worker_questions WHERE attempt=?1",
            [&q.attempt],
            |r| r.get(0),
        )?;
        ensure!(count < MAX_QUESTIONS, "attempt question limit reached");
    }
    tx.execute("INSERT INTO worker_questions VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,doc=excluded.doc", params![q.id,q.attempt,q.epoch,q.revision,serde_json::to_string(&q)?])?;
    if q.revision == 2 {
        tx.execute("INSERT INTO question_notices(question,attempt,epoch,state) VALUES (?1,?2,?3,'pending')", params![q.id,q.attempt,q.epoch])?;
    }
    Ok(())
}

pub(super) fn context(c: &Connection, order: &ExecuteOrder) -> Result<Value> {
    let mut stmt = c.prepare(
        "SELECT doc FROM worker_questions WHERE attempt=?1 AND epoch=?2 ORDER BY rowid LIMIT 33",
    )?;
    let rows = stmt
        .query_map(params![order.attempt.id, order.attempt.epoch], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        rows.len() <= MAX_QUESTIONS as usize,
        "question projection overflow"
    );
    let mut result = Vec::new();
    for raw in rows {
        let q: Question = serde_json::from_str(&raw)?;
        validate(&q, order)?;
        result.push(q);
    }
    Ok(json!(result))
}
