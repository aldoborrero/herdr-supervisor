use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::identity::{Identity, SignedRecord, verifying_key};
use super::model::*;

pub struct Store {
    pub connection: Connection,
    pub dir: PathBuf,
}

pub(super) fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        std::fs::create_dir_all(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.permissions().mode() & 0o077 == 0,
        "state directory must be private, real and mode 0700"
    );
    Ok(())
}

pub fn secret_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    std::fs::File::open(path.parent().context("secret parent")?)?.sync_all()?;
    Ok(())
}

impl Store {
    pub fn initialize(dir: &Path) -> Result<Self> {
        private_dir(dir)?;
        let _lock = super::api::lock(dir, "bootstrap.lock")?;
        // Resume a partially committed bootstrap without rotating its identity or token.
        let identity = if dir.join("owner.key").exists() {
            Identity::load(&dir.join("owner.key"))?
        } else {
            let identity = Identity::generate()?;
            identity.save_new(&dir.join("owner.key"))?;
            identity
        };
        let credential = if dir.join("admin.token").exists() {
            let meta = std::fs::symlink_metadata(dir.join("admin.token"))?;
            ensure!(
                meta.is_file() && meta.permissions().mode() & 0o077 == 0,
                "admin credential must be private and regular"
            );
            std::fs::read_to_string(dir.join("admin.token"))?
        } else {
            let credential = token()?;
            secret_file(&dir.join("admin.token"), credential.as_bytes())?;
            credential
        };
        let mut s = Self::open(dir)?;
        let tx = s
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))
            .optional()?;
        if let Some(owner) = existing {
            ensure!(owner == identity.did(), "owner key does not match database");
            let p = authenticate(&tx, &credential)?;
            own(&p)?;
        } else {
            tx.execute("INSERT INTO meta VALUES ('owner',?1)", [identity.did()])?;
            tx.execute(
                "INSERT INTO principals VALUES (?1,?2,'owner',NULL,NULL,NULL,1,1)",
                params![id(), digest(credential.as_bytes())],
            )?;
        }
        tx.commit()?;
        Ok(s)
    }

    pub fn open(dir: &Path) -> Result<Self> {
        private_dir(dir)?;
        let path = dir.join("control.sqlite");
        if path.exists() {
            ensure!(
                std::fs::symlink_metadata(&path)?.is_file(),
                "database is not a regular file"
            );
        } else {
            secret_file(&path, b"")?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                tx.execute_batch(include_str!("schema.sql"))?;
                tx.execute_batch("ALTER TABLE outbox ADD COLUMN wire TEXT;")?;
                tx.pragma_update(None, "user_version", 2)?;
            }
            1 => {
                tx.execute_batch("ALTER TABLE outbox ADD COLUMN wire TEXT;")?;
                tx.pragma_update(None, "user_version", 2)?;
            }
            2 => (),
            3 => (),
            4 => (),
            5 => (),
            _ => bail!("unsupported controller schema {version}; no migration applied"),
        }
        if version <= 2 {
            tx.execute_batch(include_str!("migration3.sql"))?;
            tx.pragma_update(None, "user_version", 3)?;
        }
        if version <= 3 {
            tx.execute_batch(include_str!("migration4.sql"))?;
            tx.pragma_update(None, "user_version", 4)?;
        }
        if version <= 4 {
            tx.execute_batch(include_str!("migration5.sql"))?;
            tx.pragma_update(None, "user_version", 5)?;
        }
        tx.commit()?;
        Ok(Self {
            connection,
            // Native processes change cwd; persist absolute controller paths.
            dir: std::fs::canonicalize(dir)?,
        })
    }

    pub fn owner(&self) -> Result<String> {
        Ok(self
            .connection
            .query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?)
    }

    pub fn apply(&mut self, request: &Request) -> Result<Value> {
        let _launch_fence = super::coordinator_launch::fence(&self.dir, &request.command)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let response = apply_transaction(&tx, request)?;
        tx.commit()?;
        Ok(response)
    }

    /// The Projects facade cannot turn an owner credential into a cross-run
    /// mutation. Validate scope against retained records in the same transaction
    /// as operation replay, including proposals already approved/withdrawn.
    pub fn apply_in_run(&mut self, run: &str, request: &Request) -> Result<Value> {
        let _launch_fence = super::coordinator_launch::fence(&self.dir, &request.command)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = authenticate(&tx, &request.token)?;
        supervise(&principal, run)?;
        let target = match &request.command {
            Mutation::Pause { run, .. }
            | Mutation::Resume { run, .. }
            | Mutation::Cancel { run, .. }
            | Mutation::CompleteRun { run, .. }
            | Mutation::AcknowledgeNotices { run, .. }
            | Mutation::WithdrawMemory { run, .. }
            | Mutation::ProposeMemory { run, .. }
            | Mutation::CreateTask { run, .. } => run.clone(),
            Mutation::StartCoordinator { run, .. } => run.clone(),
            command => {
                let (table, id) = match command {
                    Mutation::StopAttempt { attempt, .. }
                    | Mutation::RequestVerification { attempt, .. } => ("attempts", attempt),
                    Mutation::RetryTask { task, .. } | Mutation::Dispatch { task, .. } => {
                        ("tasks", task)
                    }
                    Mutation::RequestReview { integration, .. }
                    | Mutation::ApplyIntegration { integration, .. } => {
                        ("integrations", integration)
                    }
                    Mutation::ApproveMemory { proposal, .. } => ("memory_proposals", proposal),
                    _ => bail!("decision is outside the Projects facade"),
                };
                tx.query_row(&format!("SELECT run FROM {table} WHERE id=?1"), [id], |r| {
                    r.get::<_, String>(0)
                })?
            }
        };
        ensure!(
            target == run,
            "decision is outside this managed project's scope"
        );
        let response = apply_transaction(&tx, request)?;
        tx.commit()?;
        Ok(response)
    }

    /// A verified node forwards scoped mutations. Persist record and effects before broker ACK.
    pub fn receive(&mut self, signed: &SignedRecord) -> Result<Value> {
        signed.verify(now())?;
        let record = &signed.record;
        let owner = self.owner()?;
        ensure!(
            record.owner == owner
                && record.recipient == owner
                && record.record_type == "local.herdr.supervisor.event",
            "wrong event authority, type or recipient"
        );
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let node_id: String = tx
            .query_row(
                "SELECT id FROM nodes WHERE key=?1 AND epoch=?2 AND active=1",
                params![record.author, record.epoch],
                |r| r.get(0),
            )
            .optional()?
            .context("node key is not enrolled at this epoch")?;
        let request: Request = serde_json::from_value(record.payload.clone())?;
        let existing: Option<String> = tx.query_row(
            "SELECT content_hash FROM inbox WHERE owner=?1 AND author=?2 AND id=?3 AND applied=1",
            params![record.owner, record.author, record.id], |r| r.get(0),
        ).optional()?;
        if let Some(hash) = existing {
            ensure!(hash == signed.content_hash, "record ID collision");
            // A previously accepted, byte-identical broker redelivery is a receipt
            // lookup, not a new use of a retired worker capability. Node authority
            // and signature were checked above; no mutation is executed again.
            let response: String = tx.query_row(
                "SELECT o.response FROM operations o JOIN principals p ON p.id=o.principal WHERE p.token_hash=?1 AND o.id=?2",
                params![digest(request.token.as_bytes()),request.operation_id], |r|r.get(0),
            )?;
            return Ok(serde_json::from_str(&response)?);
        }
        let p = authenticate(&tx, &request.token)?;
        let attempt_id = match &request.command {
            Mutation::BindResource { attempt, .. } | Mutation::ObserveStopped { attempt, .. } => {
                ensure!(
                    p.role == "node" && p.node.as_deref() == Some(&node_id),
                    "foreign node capability"
                );
                attempt
            }
            Mutation::WorkerReady { attempt, .. }
            | Mutation::SubmitResult { attempt, .. }
            | Mutation::SubmitReview { attempt, .. } => {
                ensure!(
                    ["worker", "reviewer"].contains(&p.role.as_str())
                        && p.attempt.as_deref() == Some(attempt),
                    "foreign worker capability"
                );
                attempt
            }
            Mutation::ProposeMemory {
                source_attempt: Some(attempt),
                ..
            } => {
                ensure!(
                    p.role == "worker" && p.attempt.as_deref() == Some(attempt),
                    "foreign memory capability"
                );
                attempt
            }
            _ => bail!("nodes cannot forward controller decisions"),
        };
        let attempt: Attempt = document(&tx, "attempts", attempt_id)?;
        ensure!(attempt.node == node_id, "attempt belongs to another node");
        let response = apply_transaction(&tx, &request)?;
        tx.execute(
            "INSERT OR IGNORE INTO inbox VALUES (?1,?2,?3,?4,?5,?6,1)",
            params![
                record.owner,
                record.author,
                record.id,
                signed.content_hash,
                serde_json::to_string(signed)?,
                now()
            ],
        )?;
        effect(
            &tx,
            &node_id,
            "receipt",
            &json!({"event_id":record.id,"operation_id":request.operation_id,"result":response}),
            record.expires_at,
        )?;
        tx.commit()?;
        Ok(response)
    }

    /// Freeze the signed bytes before publishing: retry must not change timestamps or content.
    pub fn prepare_wire(&mut self, effect_id: &str, identity: &Identity) -> Result<SignedRecord> {
        ensure!(identity.did() == self.owner()?, "wrong controller key");
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (target,kind,payload,wire,deadline):(String,String,String,Option<String>,i64)=tx.query_row("SELECT target,kind,payload,wire,expires_at FROM outbox WHERE id=?1 AND state='pending'",[effect_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
        ensure!(deadline > now(), "effect expired");
        if let Some(wire) = wire {
            return Ok(serde_json::from_str(&wire)?);
        }
        let (key, epoch): (String, u64) = tx.query_row(
            "SELECT key,epoch FROM nodes WHERE id=?1 AND active=1",
            [&target],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let record = identity.sign(super::identity::Record {
            record_type: "local.herdr.supervisor.command".into(),
            id: effect_id.into(),
            owner: identity.did(),
            author: identity.did(),
            recipient: key,
            epoch,
            created_at: now(),
            expires_at: deadline,
            payload: json!({"kind":kind,"body":serde_json::from_str::<Value>(&payload)?}),
        })?;
        tx.execute(
            "UPDATE outbox SET wire=?2 WHERE id=?1",
            params![effect_id, serde_json::to_string(&record)?],
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn published(&mut self, effect_id: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE outbox SET state='broker_stored' WHERE id=?1 AND state='pending'",
            [effect_id],
        )?;
        Ok(())
    }

    pub fn nodes(&self) -> Result<Vec<String>> {
        let mut q = self
            .connection
            .prepare("SELECT id FROM nodes WHERE active=1 ORDER BY id")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn snapshot(&self, credential: &str, run_id: &str) -> Result<Value> {
        let principal = authenticate(&self.connection, credential)?;
        ensure!(
            principal.role == "owner" || principal.run.as_deref() == Some(run_id),
            "run access denied"
        );
        let run: Run = document(&self.connection, "runs", run_id)?;
        let active_attempts: u64 = self.connection.query_row(
            "SELECT count(*) FROM attempts WHERE run=?1 AND active=1",
            [run_id],
            |r| r.get(0),
        )?;
        let mut statement = self
            .connection
            .prepare("SELECT doc FROM tasks WHERE run=?1 ORDER BY id")?;
        let tasks: Vec<Value> = statement
            .query_map([run_id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        let mut statement = self
            .connection
            .prepare("SELECT doc FROM attempts WHERE run=?1 ORDER BY id")?;
        let attempts: Vec<Value> = statement
            .query_map([run_id], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        let mut query = self
            .connection
            .prepare("SELECT doc FROM integrations WHERE run=?1 ORDER BY rowid")?;
        let integrations: Vec<Value> = query
            .query_map([run_id], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<_>>()?;
        let mut query=self.connection.prepare("SELECT c.doc FROM check_runs c JOIN integrations i ON i.id=c.integration WHERE i.run=?1 ORDER BY c.rowid")?;
        let checks: Vec<Value> = query
            .query_map([run_id], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<_>>()?;
        let mut query=self.connection.prepare("SELECT v.doc FROM reviews v JOIN tasks t ON t.id=v.task WHERE t.run=?1 ORDER BY v.rowid")?;
        let reviews: Vec<Value> = query
            .query_map([run_id], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect::<Result<_>>()?;
        Ok(
            json!({"run":run,"active_attempts":active_attempts,"tasks":tasks,"attempts":attempts,"integrations":integrations,"checks":checks,"reviews":reviews}),
        )
    }

    pub fn pending(&self, credential: &str, target: &str) -> Result<Vec<Effect>> {
        let p = authenticate(&self.connection, credential)?;
        ensure!(
            p.role == "owner" || p.node.as_deref() == Some(target),
            "outbox access denied"
        );
        let mut query = self.connection.prepare("SELECT id,target,kind,payload,state FROM outbox WHERE target=?1 AND state='pending' AND available_at<=?2 AND expires_at>?2 ORDER BY rowid LIMIT 100")?;
        let rows = query.query_map(params![target, now()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|r| {
            let (id, target, kind, payload, state) = r?;
            Ok(Effect {
                id,
                target,
                kind,
                payload: serde_json::from_str(&payload)?,
                state,
            })
        })
        .collect()
    }

    pub fn memory(
        &self,
        credential: &str,
        run: &str,
        topics: &[String],
        budget: usize,
    ) -> Result<Value> {
        let p = authenticate(&self.connection, credential)?;
        ensure!(
            p.role == "owner" || p.run.as_deref() == Some(run),
            "memory access denied"
        );
        memory_snapshot(&self.connection, run, topics, budget.min(32000))
    }
}

fn apply_transaction(tx: &Transaction<'_>, request: &Request) -> Result<Value> {
    identifier(&request.operation_id)?;
    let bytes = serde_json::to_vec(&request.command)?;
    ensure!(bytes.len() <= 262144, "request too large");
    let fingerprint = digest(&bytes);
    let principal = authenticate(tx, &request.token)?;
    let replay: Option<(String, String)> = tx
        .query_row(
            "SELECT request_hash,response FROM operations WHERE principal=?1 AND id=?2",
            params![principal.id, request.operation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((hash, response)) = replay {
        ensure!(
            hash == fingerprint,
            "operation ID reused with different content"
        );
        return Ok(serde_json::from_str(&response)?);
    }
    let response = mutate(tx, &principal, &request.command)?;
    tx.execute(
        "INSERT INTO operations VALUES (?1,?2,?3,?4)",
        params![
            principal.id,
            request.operation_id,
            fingerprint,
            serde_json::to_string(&response)?
        ],
    )?;
    tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES (?1,?2,?3,?4,?5,?6)", params![principal.id, request.operation_id, response.get("id").and_then(Value::as_str).unwrap_or(""), serde_json::to_value(&request.command)?["method"].as_str().unwrap_or(""), serde_json::to_string(&request.command)?, now()])?;
    Ok(response)
}

pub fn authenticate(c: &Connection, token: &str) -> Result<Principal> {
    ensure!(
        token.len() <= 256 && token.len() >= 32,
        "invalid credential"
    );
    c.query_row("SELECT id,role,run,attempt,node,generation FROM principals WHERE token_hash=?1 AND active=1", [digest(token.as_bytes())], |r| Ok(Principal { id:r.get(0)?,role:r.get(1)?,run:r.get(2)?,attempt:r.get(3)?,node:r.get(4)?,generation:r.get(5)? })).optional()?.context("invalid or retired credential")
}

pub fn document<T: DeserializeOwned>(c: &Connection, table: &str, id: &str) -> Result<T> {
    ensure!(
        ["runs", "tasks", "attempts"].contains(&table),
        "internal table error"
    );
    let text: String = c
        .query_row(&format!("SELECT doc FROM {table} WHERE id=?1"), [id], |r| {
            r.get(0)
        })
        .optional()?
        .with_context(|| format!("unknown {table} ID"))?;
    Ok(serde_json::from_str(&text)?)
}

pub(super) fn own(p: &Principal) -> Result<()> {
    ensure!(p.role == "owner", "owner capability required");
    Ok(())
}
pub(super) fn supervise(p: &Principal, run: &str) -> Result<()> {
    ensure!(
        p.role == "owner" || p.role == "supervisor" && p.run.as_deref() == Some(run),
        "supervisor scope denied"
    );
    Ok(())
}
pub(super) fn worker(p: &Principal, a: &Attempt, epoch: u64) -> Result<()> {
    ensure!(
        ["worker", "reviewer"].contains(&p.role.as_str())
            && p.role == a.role
            && p.attempt.as_deref() == Some(&a.id)
            && p.generation == epoch
            && a.epoch == epoch,
        "worker scope or epoch mismatch"
    );
    ensure!(
        !["retired", "cancelling", "stopped"].contains(&a.state.as_str()),
        "attempt fenced"
    );
    Ok(())
}
fn node(p: &Principal, a: &Attempt, epoch: u64) -> Result<()> {
    ensure!(
        p.role == "node" && p.node.as_deref() == Some(&a.node) && epoch == a.epoch,
        "node scope or epoch mismatch"
    );
    Ok(())
}
pub(super) fn revision(actual: u64, expected: u64) -> Result<()> {
    ensure!(
        actual == expected,
        "stale revision: expected {expected}, current {actual}"
    );
    Ok(())
}
pub(super) fn active_run(run: &Run) -> Result<()> {
    ensure!(
        run.state == "active" && now() < run.deadline,
        "run is paused, expired or closed"
    );
    Ok(())
}

pub(super) fn save_run(c: &Connection, run: &Run) -> Result<()> {
    c.execute(
        "UPDATE runs SET state=?2,revision=?3,doc=?4 WHERE id=?1",
        params![run.id, run.state, run.revision, serde_json::to_string(run)?],
    )?;
    Ok(())
}
pub(super) fn save_task(c: &Connection, task: &Task) -> Result<()> {
    c.execute(
        "UPDATE tasks SET state=?2,revision=?3,doc=?4 WHERE id=?1",
        params![
            task.id,
            task.state,
            task.revision,
            serde_json::to_string(task)?
        ],
    )?;
    Ok(())
}
pub(super) fn save_attempt(c: &Connection, a: &Attempt) -> Result<()> {
    c.execute(
        "UPDATE attempts SET state=?2,epoch=?3,doc=?4 WHERE id=?1",
        params![a.id, a.state, a.epoch, serde_json::to_string(a)?],
    )?;
    Ok(())
}
pub(super) fn issue(
    c: &Connection,
    role: &str,
    run: Option<&str>,
    attempt: Option<&str>,
    node: Option<&str>,
    generation: u64,
) -> Result<String> {
    let secret = token()?;
    c.execute(
        "INSERT INTO principals VALUES (?1,?2,?3,?4,?5,?6,?7,1)",
        params![
            id(),
            digest(secret.as_bytes()),
            role,
            run,
            attempt,
            node,
            generation
        ],
    )?;
    Ok(secret)
}

pub(super) fn effect(
    c: &Connection,
    target: &str,
    kind: &str,
    payload: &Value,
    deadline: i64,
) -> Result<String> {
    let effect_id = id();
    c.execute("INSERT INTO outbox(id,target,kind,payload,available_at,expires_at) VALUES (?1,?2,?3,?4,?5,?6)", params![effect_id,target,kind,serde_json::to_string(payload)?,now(),deadline])?;
    Ok(effect_id)
}

fn add_dependency(c: &Connection, task: &Task, predecessor: &str) -> Result<()> {
    let pred: Task = document(c, "tasks", predecessor)?;
    ensure!(
        task.run == pred.run && task.id != pred.id,
        "invalid dependency"
    );
    let cycle: bool = c.query_row("WITH RECURSIVE reachable(id) AS (SELECT ?1 UNION SELECT d.predecessor FROM dependencies d JOIN reachable r ON d.task=r.id) SELECT EXISTS(SELECT 1 FROM reachable WHERE id=?2)", params![predecessor,task.id], |r| r.get(0))?;
    ensure!(!cycle, "dependency cycle");
    c.execute(
        "INSERT OR IGNORE INTO dependencies VALUES (?1,?2)",
        params![task.id, predecessor],
    )?;
    Ok(())
}

pub(super) fn mutate(tx: &Transaction<'_>, p: &Principal, command: &Mutation) -> Result<Value> {
    match command {
        Mutation::StartCoordinator { .. } => super::coordinator_launch::reserve(tx, p, command),
        Mutation::BindCoordinator { .. } | Mutation::AcknowledgeNotices { .. } => {
            super::coordinator::mutate(tx, p, command)
        }
        Mutation::RequestVerification { .. }
        | Mutation::RequestReview { .. }
        | Mutation::SubmitReview { .. }
        | Mutation::ApplyIntegration { .. } => super::verification::mutate(tx, p, command),
        Mutation::EnrollNode {
            id: node_id,
            key,
            max_active,
        } => {
            own(p)?;
            identifier(node_id)?;
            verifying_key(key)?;
            ensure!(
                *max_active > 0 && *max_active <= 128,
                "invalid node capacity"
            );
            tx.execute(
                "INSERT INTO nodes VALUES (?1,?2,1,?3,1)",
                params![node_id, key, max_active],
            )?;
            let credential = issue(tx, "node", None, None, Some(node_id), 1)?;
            Ok(json!({"id":node_id,"epoch":1,"credential":credential}))
        }
        Mutation::CreateRun {
            project,
            repo,
            base_oid,
            objective,
            policy,
        } => {
            own(p)?;
            identifier(project)?;
            oid(base_oid)?;
            policy.validate()?;
            ensure!(
                Path::new(repo).is_absolute() && !objective.trim().is_empty(),
                "absolute repository and objective required"
            );
            let run = Run {
                id: id(),
                project: project.clone(),
                repo: repo.clone(),
                base_oid: base_oid.clone(),
                integration_oid: base_oid.clone(),
                objective: objective.clone(),
                policy: policy.clone(),
                state: "active".into(),
                revision: 1,
                deadline: now() + policy.duration_secs as i64,
            };
            tx.execute(
                "INSERT INTO runs VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    run.id,
                    run.project,
                    run.repo,
                    run.state,
                    run.revision,
                    serde_json::to_string(&run)?
                ],
            )?;
            Ok(serde_json::to_value(run)?)
        }
        Mutation::BindSupervisor {
            run,
            expected_revision,
        } => {
            own(p)?;
            let mut r: Run = document(tx, "runs", run)?;
            revision(r.revision, *expected_revision)?;
            tx.execute("UPDATE coordinator_routes SET active=0 WHERE run=?1", [run])?;
            tx.execute("UPDATE coordinator_notices SET state='superseded' WHERE run=?1 AND state IN ('pending','retry')",[run])?;
            let generation: u64 = tx.query_row("SELECT COALESCE(MAX(generation),0)+1 FROM principals WHERE role='supervisor' AND run=?1", [run], |r|r.get(0))?;
            tx.execute(
                "UPDATE principals SET active=0 WHERE role='supervisor' AND run=?1",
                [run],
            )?;
            let credential = issue(tx, "supervisor", Some(run), None, None, generation)?;
            r.revision += 1;
            save_run(tx, &r)?;
            Ok(
                json!({"id":run,"generation":generation,"revision":r.revision,"credential":credential}),
            )
        }
        Mutation::CreateTask {
            run,
            expected_revision,
            contract,
            dependencies,
        } => {
            supervise(p, run)?;
            let mut r: Run = document(tx, "runs", run)?;
            revision(r.revision, *expected_revision)?;
            active_run(&r)?;
            contract.validate(&r.policy)?;
            let task = Task {
                id: id(),
                run: run.clone(),
                contract: contract.clone(),
                contract_hash: digest(&serde_json::to_vec(contract)?),
                state: "planned".into(),
                revision: 1,
            };
            tx.execute(
                "INSERT INTO tasks VALUES (?1,?2,?3,?4,?5)",
                params![
                    task.id,
                    task.run,
                    task.state,
                    task.revision,
                    serde_json::to_string(&task)?
                ],
            )?;
            for pred in dependencies {
                add_dependency(tx, &task, pred)?;
            }
            r.revision += 1;
            save_run(tx, &r)?;
            Ok(serde_json::to_value(task)?)
        }
        Mutation::AddDependency {
            task,
            expected_revision,
            predecessor,
        } => {
            let mut t: Task = document(tx, "tasks", task)?;
            supervise(p, &t.run)?;
            revision(t.revision, *expected_revision)?;
            ensure!(t.state == "planned", "contract already dispatched");
            let r: Run = document(tx, "runs", &t.run)?;
            active_run(&r)?;
            add_dependency(tx, &t, predecessor)?;
            t.revision += 1;
            save_task(tx, &t)?;
            Ok(serde_json::to_value(t)?)
        }
        Mutation::Dispatch {
            task,
            expected_revision,
            node,
            profile,
        } => {
            let mut t: Task = document(tx, "tasks", task)?;
            supervise(p, &t.run)?;
            revision(t.revision, *expected_revision)?;
            let r: Run = document(tx, "runs", &t.run)?;
            active_run(&r)?;
            ensure!(t.state == "planned", "task not dispatchable");
            let incomplete: i64 = tx.query_row("SELECT count(*) FROM dependencies d JOIN tasks t ON t.id=d.predecessor WHERE d.task=?1 AND t.state NOT IN ('integrated','completed')",[task],|r|r.get(0))?;
            ensure!(incomplete == 0, "dependencies not integrated");
            let prof = r
                .policy
                .profiles
                .get(profile)
                .context("profile not allowed")?;
            let node_limit: u32 = tx
                .query_row(
                    "SELECT max_active FROM nodes WHERE id=?1 AND active=1",
                    [node],
                    |r| r.get(0),
                )
                .optional()?
                .context("node not enrolled")?;
            let (active,workers): (u32,u32) = tx.query_row("SELECT count(*),COALESCE(SUM(role='worker'),0) FROM attempts WHERE run=?1 AND active=1",[&t.run],|r|Ok((r.get(0)?,r.get(1)?)))?;
            ensure!(
                active < r.policy.max_active
                    && workers < r.policy.max_active - r.policy.reserved_reviewers,
                "run capacity exhausted"
            );
            let on_node: u32 = tx.query_row(
                "SELECT count(*) FROM attempts WHERE node=?1 AND active=1",
                [node],
                |r| r.get(0),
            )?;
            ensure!(on_node < node_limit, "node capacity exhausted");
            let on_profile: u32 = tx.query_row(
                "SELECT count(*) FROM attempts WHERE run=?1 AND profile=?2 AND active=1",
                params![t.run, profile],
                |r| r.get(0),
            )?;
            ensure!(on_profile < prof.limit, "profile capacity exhausted");
            let (number, worker_count): (u64,u64) = tx.query_row(
                "SELECT COALESCE(MAX(number),0)+1,COALESCE(SUM(role='worker'),0) FROM attempts WHERE task=?1",
                [task],
                |r| Ok((r.get(0)?,r.get(1)?)),
            )?;
            ensure!(
                worker_count < r.policy.max_attempts as u64,
                "attempt budget exhausted"
            );
            let a = Attempt {
                id: id(),
                task: t.id.clone(),
                run: t.run.clone(),
                number,
                epoch: 1,
                node: node.clone(),
                profile: profile.clone(),
                role: "worker".into(),
                base_oid: r.integration_oid.clone(),
                contract_hash: t.contract_hash.clone(),
                state: "reserved".into(),
                native_session: None,
                resource: None,
            };
            tx.execute(
                "INSERT INTO attempts VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)",
                params![
                    a.id,
                    a.task,
                    a.run,
                    a.node,
                    a.profile,
                    a.role,
                    a.number,
                    a.epoch,
                    a.state,
                    serde_json::to_string(&a)?
                ],
            )?;
            let credential = issue(tx, "worker", Some(&a.run), Some(&a.id), None, a.epoch)?;
            let memory = memory_snapshot(tx, &r.id, &t.contract.memory_topics, 32000)?;
            effect(
                tx,
                node,
                "execute",
                &json!({"attempt":a,"contract":t.contract,"profile":prof,"repo":r.repo,"credential":credential,"memory":memory,"memory_version":super::live_memory::version(tx,&r.id)?}),
                r.deadline,
            )?;
            t.state = "executing".into();
            t.revision += 1;
            save_task(tx, &t)?;
            Ok(serde_json::to_value(a)?)
        }
        Mutation::BindResource {
            attempt,
            epoch,
            resource,
        } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            node(p, &a, *epoch)?;
            ensure!(
                a.state == "reserved",
                "resource already bound or attempt fenced"
            );
            let r: Run = document(tx, "runs", &a.run)?;
            ensure!(
                ["active", "paused"].contains(&r.state.as_str()),
                "run cancelled"
            );
            ensure!(
                resource.runtime == r.policy.profiles[&a.profile].runtime,
                "runtime mismatch"
            );
            uuid::Uuid::parse_str(&resource.session)?;
            ensure!(
                Path::new(&resource.worktree).is_absolute()
                    && Path::new(&resource.repo_common_dir).is_absolute(),
                "resource paths must be absolute"
            );
            ensure!(
                !resource.pane.is_empty()
                    && !resource.terminal.is_empty()
                    && !resource.workspace.is_empty()
                    && !resource.mailer_agent.is_empty()
                    && resource.mailer_epoch > 0,
                "incomplete runtime binding"
            );
            ensure!(
                resource.branch == format!("hs/{}/{}", a.run, a.id),
                "unexpected branch"
            );
            tx.execute(
                "INSERT INTO resources VALUES (?1,?2,?3,?4,?5)",
                params![
                    attempt,
                    a.node,
                    resource.worktree,
                    resource.pane,
                    resource.session
                ],
            )?;
            a.resource = Some(resource.clone());
            a.native_session = Some(resource.session.clone());
            a.state = "awaiting_ready".into();
            save_attempt(tx, &a)?;
            Ok(serde_json::to_value(a)?)
        }
        Mutation::WorkerReady {
            attempt,
            epoch,
            session,
        } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            worker(p, &a, *epoch)?;
            ensure!(
                a.state == "awaiting_ready" && a.native_session.as_deref() == Some(session),
                "worker session not bound"
            );
            let r: Run = document(tx, "runs", &a.run)?;
            ensure!(
                ["active", "paused"].contains(&r.state.as_str()) && now() < r.deadline,
                "run closed or expired"
            );
            a.state = "running".into();
            save_attempt(tx, &a)?;
            Ok(serde_json::to_value(a)?)
        }
        Mutation::SubmitResult {
            attempt,
            epoch,
            contract_hash,
            commit,
            artifacts,
            summary,
        } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            worker(p, &a, *epoch)?;
            ensure!(a.role == "worker", "reviewers cannot submit author results");
            ensure!(
                a.state == "running" && a.contract_hash == *contract_hash,
                "result is stale or attempt not ready"
            );
            let mut t: Task = document(tx, "tasks", &a.task)?;
            let r: Run = document(tx, "runs", &a.run)?;
            ensure!(
                ["active", "paused"].contains(&r.state.as_str()),
                "run cancelled"
            );
            ensure!(
                t.contract_hash == *contract_hash && !summary.trim().is_empty(),
                "contract mismatch or empty result"
            );
            if t.contract.kind == "code" {
                oid(commit.as_deref().context("code result needs commit")?)?;
            } else {
                ensure!(
                    !artifacts.is_empty(),
                    "artifact result needs content hashes"
                );
            }
            for (name, hash) in artifacts {
                identifier(name)?;
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
                    "invalid artifact hash"
                );
            }
            let result = json!({"id":attempt,"task":a.task,"contract_hash":contract_hash,"commit":commit,"artifacts":artifacts,"summary":summary,"author":p.id});
            tx.execute(
                "INSERT INTO results VALUES (?1,?2,?3,?4,?5)",
                params![
                    attempt,
                    a.task,
                    contract_hash,
                    commit,
                    serde_json::to_string(&result)?
                ],
            )?;
            a.state = "result_submitted".into();
            save_attempt(tx, &a)?;
            t.state = "awaiting_review".into();
            t.revision += 1;
            save_task(tx, &t)?;
            effect(tx, "controller", "result_submitted", &result, now() + 86400)?;
            // A submitted result does not prove the worker process has stopped.
            Ok(result)
        }
        Mutation::Pause {
            run,
            expected_revision,
        }
        | Mutation::Resume {
            run,
            expected_revision,
        }
        | Mutation::Cancel {
            run,
            expected_revision,
        } => {
            supervise(p, run)?;
            let mut r: Run = document(tx, "runs", run)?;
            revision(r.revision, *expected_revision)?;
            match command {
                Mutation::Pause { .. } => {
                    ensure!(r.state == "active", "run not active");
                    r.state = "paused".into();
                }
                Mutation::Resume { .. } => {
                    ensure!(
                        r.state == "paused" && now() < r.deadline,
                        "run not resumable"
                    );
                    r.state = "active".into();
                }
                _ => {
                    ensure!(
                        ["active", "paused"].contains(&r.state.as_str()),
                        "run already closed"
                    );
                    r.state = "cancelling".into();
                    let mut query =
                        tx.prepare("SELECT doc FROM attempts WHERE run=?1 AND active=1")?;
                    let attempts: Vec<String> = query
                        .query_map([run], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    for text in &attempts {
                        let mut a: Attempt = serde_json::from_str(text)?;
                        a.epoch += 1;
                        a.state = "cancelling".into();
                        save_attempt(tx, &a)?;
                        tx.execute("UPDATE principals SET active=0 WHERE attempt=?1", [&a.id])?;
                        effect(tx, &a.node, "cancel", &json!({"attempt":a}), now() + 86400)?;
                    }
                    if attempts.is_empty() {
                        r.state = "cancelled".into();
                    }
                    let mut query=tx.prepare("SELECT doc FROM tasks WHERE run=?1 AND state NOT IN ('completed','integrated','cancelled')")?;
                    let tasks: Vec<String> = query
                        .query_map([run], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    for text in tasks {
                        let mut t: Task = serde_json::from_str(&text)?;
                        t.state = if attempts.is_empty() {
                            "cancelled"
                        } else {
                            "cancelling"
                        }
                        .into();
                        t.revision += 1;
                        save_task(tx, &t)?;
                    }
                }
            }
            r.revision += 1;
            save_run(tx, &r)?;
            Ok(serde_json::to_value(r)?)
        }
        Mutation::RetryTask {
            task,
            expected_revision,
            contract,
        } => {
            let mut t: Task = document(tx, "tasks", task)?;
            supervise(p, &t.run)?;
            revision(t.revision, *expected_revision)?;
            let r: Run = document(tx, "runs", &t.run)?;
            active_run(&r)?;
            ensure!(
                ["interrupted", "changes_requested"].contains(&t.state.as_str()),
                "task does not need a retry"
            );
            let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM attempts WHERE task=?1 AND active=1) OR EXISTS(SELECT 1 FROM integrations WHERE task=?1 AND state IN ('preparing','checking','applying'))",[task],|r|r.get(0))?;
            ensure!(
                !busy,
                "prior processes or verification effects remain unresolved"
            );
            if let Some(contract) = contract {
                contract.validate(&r.policy)?;
                t.contract = contract.clone();
                t.contract_hash = digest(&serde_json::to_vec(contract)?);
            }
            let mut q =
                tx.prepare("SELECT doc FROM integrations WHERE task=?1 AND state<>'applied'")?;
            let candidates: Vec<String> = q
                .query_map([task], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for doc in candidates {
                let mut i: Integration = serde_json::from_str(&doc)?;
                i.state = "superseded".into();
                i.revision += 1;
                super::verification::save(tx, &i)?;
            }
            t.state = "planned".into();
            t.revision += 1;
            save_task(tx, &t)?;
            Ok(serde_json::to_value(t)?)
        }
        Mutation::StopAttempt {
            attempt,
            expected_epoch,
        } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            supervise(p, &a.run)?;
            revision(a.epoch, *expected_epoch)?;
            ensure!(
                !["stopped", "cancelling"].contains(&a.state.as_str()),
                "attempt already stopping or stopped"
            );
            a.epoch += 1;
            a.state = "cancelling".into();
            save_attempt(tx, &a)?;
            tx.execute("UPDATE principals SET active=0 WHERE attempt=?1", [attempt])?;
            effect(tx, &a.node, "cancel", &json!({"attempt":a}), now() + 86400)?;
            Ok(serde_json::to_value(a)?)
        }
        Mutation::CompleteRun {
            run,
            expected_revision,
        } => {
            supervise(p, run)?;
            let mut r: Run = document(tx, "runs", run)?;
            revision(r.revision, *expected_revision)?;
            ensure!(
                ["active", "paused"].contains(&r.state.as_str()),
                "run already closed"
            );
            let unfinished: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks WHERE run=?1 AND state NOT IN ('integrated','completed')) OR EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND active=1) OR EXISTS(SELECT 1 FROM integrations WHERE run=?1 AND state IN ('preparing','checking','awaiting_review','verified','applying'))",[run],|r|r.get(0))?;
            ensure!(
                !unfinished,
                "tasks, processes or integration intents remain unfinished"
            );
            r.state = "completed".into();
            r.revision += 1;
            save_run(tx, &r)?;
            tx.execute(
                "UPDATE repository_bindings SET active=0 WHERE run=?1",
                [run],
            )?;
            Ok(serde_json::to_value(r)?)
        }
        Mutation::ObserveStopped { attempt, epoch } => {
            let mut a: Attempt = document(tx, "attempts", attempt)?;
            node(p, &a, *epoch)?;
            ensure!(a.state != "stopped", "already stopped");
            a.state = "stopped".into();
            save_attempt(tx, &a)?;
            tx.execute("UPDATE attempts SET active=0 WHERE id=?1", [attempt])?;
            tx.execute("UPDATE principals SET active=0 WHERE attempt=?1", [attempt])?;
            let mut task: Task = document(tx, "tasks", &a.task)?;
            if a.role == "worker" && task.state == "executing" {
                task.state = "interrupted".into();
                task.revision += 1;
                save_task(tx, &task)?;
            }
            let mut r: Run = document(tx, "runs", &a.run)?;
            if r.state == "cancelling" {
                let remaining: u32 = tx.query_row(
                    "SELECT count(*) FROM attempts WHERE run=?1 AND active=1",
                    [&a.run],
                    |r| r.get(0),
                )?;
                if remaining == 0 {
                    r.state = "cancelled".into();
                    r.revision += 1;
                    save_run(tx, &r)?;
                    let mut query =
                        tx.prepare("SELECT doc FROM tasks WHERE run=?1 AND state='cancelling'")?;
                    let tasks: Vec<String> = query
                        .query_map([&r.id], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    for text in tasks {
                        let mut t: Task = serde_json::from_str(&text)?;
                        t.state = "cancelled".into();
                        t.revision += 1;
                        save_task(tx, &t)?;
                    }
                }
            }
            Ok(serde_json::to_value(a)?)
        }
        Mutation::ProposeMemory {
            run,
            topic,
            text,
            source_attempt,
            evidence,
        } => {
            ensure!(
                p.role == "owner" || p.run.as_deref() == Some(run),
                "memory scope denied"
            );
            identifier(topic)?;
            ensure!(
                !text.trim().is_empty() && text.len() <= 32000 && !evidence.is_empty(),
                "memory needs bounded text and provenance"
            );
            let _: Run = document(tx, "runs", run)?;
            if p.role == "worker" {
                ensure!(
                    source_attempt == &p.attempt,
                    "worker may only cite its own attempt"
                );
            } else {
                supervise(p, run)?;
            }
            if let Some(source) = source_attempt {
                let a: Attempt = document(tx, "attempts", source)?;
                ensure!(a.run == *run, "foreign memory source");
            }
            let proposal = id();
            tx.execute(
                "INSERT INTO memory_proposals VALUES (?1,?2,?3,?4,?5,?6,?7,'proposed')",
                params![
                    proposal,
                    run,
                    topic,
                    p.id,
                    source_attempt,
                    text,
                    serde_json::to_string(evidence)?
                ],
            )?;
            Ok(json!({"id":proposal,"status":"proposed"}))
        }
        Mutation::ApproveMemory {
            proposal,
            expected_revision,
        } => {
            let (run, topic, text, status): (String, String, String, String) = tx.query_row(
                "SELECT run,topic,text,status FROM memory_proposals WHERE id=?1",
                [proposal],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            supervise(p, &run)?;
            ensure!(status == "proposed", "proposal already decided");
            let current: u64 = tx.query_row(
                "SELECT COALESCE(MAX(revision),0) FROM memory_revisions WHERE run=?1 AND topic=?2",
                params![run, topic],
                |r| r.get(0),
            )?;
            revision(current, *expected_revision)?;
            tx.execute(
                "INSERT INTO memory_revisions VALUES (?1,?2,?3,?4,?5,?6,0)",
                params![run, topic, current + 1, proposal, text, p.id],
            )?;
            tx.execute(
                "UPDATE memory_proposals SET status='approved' WHERE id=?1",
                [proposal],
            )?;
            super::live_memory::publish(tx, &run, &topic)?;
            effect(
                tx,
                "controller",
                "memory_changed",
                &json!({"run":run,"topic":topic,"revision":current+1}),
                now() + 86400,
            )?;
            Ok(json!({"id":proposal,"topic":topic,"revision":current+1}))
        }
        Mutation::WithdrawMemory {
            run,
            topic,
            expected_revision,
        } => {
            supervise(p, run)?;
            let current: u64 = tx.query_row(
                "SELECT COALESCE(MAX(revision),0) FROM memory_revisions WHERE run=?1 AND topic=?2",
                params![run, topic],
                |r| r.get(0),
            )?;
            revision(current, *expected_revision)?;
            ensure!(current > 0, "unknown memory topic");
            tx.execute(
                "INSERT INTO memory_revisions VALUES (?1,?2,?3,NULL,'',?4,1)",
                params![run, topic, current + 1, p.id],
            )?;
            super::live_memory::publish(tx, run, topic)?;
            effect(
                tx,
                "controller",
                "memory_changed",
                &json!({"run":run,"topic":topic,"revision":current+1,"withdrawn":true}),
                now() + 86400,
            )?;
            Ok(json!({"id":topic,"revision":current+1}))
        }
    }
}

pub(super) fn memory_snapshot(
    c: &Connection,
    run: &str,
    topics: &[String],
    budget: usize,
) -> Result<Value> {
    let mut q=c.prepare("SELECT m.topic,m.revision,m.text,m.proposal FROM memory_revisions m WHERE m.run=?1 AND m.withdrawn=0 AND m.revision=(SELECT max(n.revision) FROM memory_revisions n WHERE n.run=m.run AND n.topic=m.topic) ORDER BY m.topic")?;
    let rows = q.query_map([run], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut used = 0;
    let mut included = BTreeMap::new();
    let mut omitted = Vec::new();
    for row in rows {
        let (topic, revision, text, proposal) = row?;
        if !topics.iter().any(|t| t == &topic) && topic != "preferences" {
            continue;
        }
        if used + text.len() > budget {
            omitted.push(topic);
            continue;
        }
        used += text.len();
        included.insert(topic,json!({"revision":revision,"text":text,"proposal":proposal,"hash":digest(text.as_bytes())}));
    }
    Ok(json!({"topics":included,"omitted":omitted,"bytes":used}))
}
