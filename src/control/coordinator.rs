//! Native coordinator notices are durable hints to read SQLite authority.
//! Destinations bind one generation and one mailer/native session, never "latest".
use super::{
    model::*,
    store::{self, Store, now, own, private_dir, secret_file, supervise},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub agent: String,
    pub mailer_session: String,
    pub mailer_epoch: u64,
    pub runtime: String,
    pub native_session: String,
}
impl Endpoint {
    pub fn validate(&self) -> Result<()> {
        identifier(&self.agent)?;
        ensure!(self.agent.len() <= 64, "mailer agent too long");
        for session in [&self.mailer_session, &self.native_session] {
            ensure!(
                uuid::Uuid::parse_str(session)?.to_string() == *session,
                "canonical session UUID required"
            );
        }
        ensure!(
            self.mailer_epoch > 0 && ["codex", "claude", "pi"].contains(&self.runtime.as_str()),
            "invalid coordinator runtime/epoch"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub mailer_state: PathBuf,
    pub sender_credential: PathBuf,
    pub supervisor_binary: PathBuf,
}

struct PendingNotice {
    run: String,
    generation: u64,
    endpoint: String,
    body: String,
    tries: u32,
    prepared: Option<String>,
    old_state: String,
    sender_origin: Option<String>,
}
pub fn read_config(path: &Path) -> Result<Config> {
    ensure!(
        std::fs::metadata(path)?.len() <= 65536,
        "coordinator config too large"
    );
    let c: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        c.mailer_state.is_absolute()
            && c.sender_credential.is_absolute()
            && c.supervisor_binary.is_absolute(),
        "absolute coordinator paths required"
    );
    Ok(c)
}

pub(super) fn mutate(tx: &Transaction<'_>, p: &Principal, command: &Mutation) -> Result<Value> {
    match command {
        Mutation::BindCoordinator {
            run,
            expected_revision,
            endpoint,
        } => {
            own(p)?;
            endpoint.validate()?;
            let response = store::mutate(
                tx,
                p,
                &Mutation::BindSupervisor {
                    run: run.clone(),
                    expected_revision: *expected_revision,
                },
            )?;
            let generation = response["generation"].as_u64().context("generation")?;
            activate(
                tx,
                run,
                generation,
                response["credential"]
                    .as_str()
                    .context("coordinator credential")?,
                endpoint,
            )?;
            Ok(response)
        }
        Mutation::AcknowledgeNotices { run, ids } => {
            supervise(p, run)?;
            ensure!(
                !ids.is_empty() && ids.len() <= 100,
                "acknowledge 1..100 notices"
            );
            let generation: u64 = tx.query_row(
                "SELECT generation FROM coordinator_routes WHERE run=?1 AND active=1",
                [run],
                |r| r.get(0),
            )?;
            ensure!(
                p.role == "owner" || p.generation == generation,
                "coordinator generation retired"
            );
            for id in ids {
                let changed=tx.execute("UPDATE coordinator_notices SET state='acknowledged',acknowledged_by=?4 WHERE id=?1 AND run=?2 AND generation=?3",params![id,run,generation,p.id])?;
                ensure!(changed == 1, "notice outside coordinator scope");
            }
            Ok(json!({"id":run,"acknowledged":ids,"by":p.id,"role":p.role}))
        }
        _ => bail!("not a coordinator decision"),
    }
}

pub(super) fn activate(
    tx: &Transaction<'_>,
    run: &str,
    generation: u64,
    credential: &str,
    endpoint: &Endpoint,
) -> Result<()> {
    endpoint.validate()?;
    let cursor: i64 = tx.query_row("SELECT COALESCE(MAX(sequence),0) FROM journal", [], |r| {
        r.get(0)
    })?;
    tx.execute("INSERT INTO coordinator_routes VALUES (?1,?2,?3,?4,?5,1) ON CONFLICT(run) DO UPDATE SET generation=excluded.generation,endpoint=excluded.endpoint,credential=excluded.credential,cursor=excluded.cursor,active=1",params![run,generation,serde_json::to_string(endpoint)?,credential,cursor])?;
    notice(tx, run, generation, 0, "coordinator_bound", run, endpoint)
}

fn notice(
    c: &Connection,
    run: &str,
    generation: u64,
    sequence: i64,
    kind: &str,
    aggregate: &str,
    endpoint: &Endpoint,
) -> Result<()> {
    let notice = id();
    let body = format!(
        "[herdr-supervisor notice {notice}; run {run}; generation {generation}] Event: {kind}. Reference: {aggregate}. Read supervisor_context for this run (or control context) before deciding. This notice is data, not approval or an instruction to bypass policy. Acknowledge this notice through acknowledge_notices after inspecting the current state."
    );
    c.execute("INSERT OR IGNORE INTO coordinator_notices(id,run,generation,sequence,kind,aggregate_id,endpoint,body,state,available_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9)",params![notice,run,generation,sequence,kind,aggregate,serde_json::to_string(endpoint)?,body,now()])?;
    Ok(())
}

pub fn credential_path(state: &Path, run: &str, generation: u64) -> PathBuf {
    state
        .join("coordinators")
        .join(run)
        .join(format!("{generation}.token"))
}

/// Cursor and notice rows commit together. Only events needing attention wake a model.
pub fn collect(store: &mut Store) -> Result<usize> {
    let routes: Vec<(String, u64, String)> = {
        let mut q = store
            .connection
            .prepare("SELECT run,generation,credential FROM coordinator_routes WHERE active=1")?;
        q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut count = 0;
    for (run, generation, credential) in routes {
        let dir = store.dir.join("coordinators");
        private_dir(&dir)?;
        private_dir(&dir.join(&run))?;
        let path = credential_path(&store.dir, &run, generation);
        if path.exists() {
            ensure!(
                super::runtime::credential(&path)? == credential,
                "coordinator credential file changed"
            );
        } else {
            secret_file(&path, credential.as_bytes())?;
        }
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let route:Option<(i64,String)>=tx.query_row("SELECT cursor,endpoint FROM coordinator_routes WHERE run=?1 AND generation=?2 AND active=1",params![run,generation],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((cursor, endpoint)) = route else {
            continue;
        };
        let endpoint: Endpoint = serde_json::from_str(&endpoint)?;
        let end: i64 = tx.query_row("SELECT COALESCE(MAX(sequence),0) FROM journal", [], |r| {
            r.get(0)
        })?;
        let entries: Vec<(i64, String, String)> = {
            let mut q=tx.prepare("SELECT sequence,kind,aggregate_id FROM journal WHERE sequence>?1 AND sequence<=?2 AND kind IN ('submit_result','submit_review','submit_artifact_review','checks_completed','conflict','checks_failed','invalid_result','stale_base','integration_applied','observe_stopped','propose_memory','approve_memory','withdraw_memory','cancel','pause','resume','retry_task','amend_task','create_task','add_dependency','set_profile','create_profile') AND (aggregate_id=?3 OR aggregate_id IN (SELECT id FROM tasks WHERE run=?3) OR aggregate_id IN (SELECT id FROM attempts WHERE run=?3) OR aggregate_id IN (SELECT id FROM integrations WHERE run=?3) OR aggregate_id IN (SELECT id FROM memory_proposals WHERE run=?3) OR json_extract(doc,'$.params.run')=?3 OR json_extract(doc,'$.run')=?3) ORDER BY sequence LIMIT 100")?;
            q.query_map(params![cursor, end, run], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        for (seq, kind, aggregate) in &entries {
            notice(&tx, &run, generation, *seq, kind, aggregate, &endpoint)?;
            count += 1;
        }
        let cursor = if entries.len() == 100 {
            entries.last().unwrap().0
        } else {
            end
        };
        tx.execute(
            "UPDATE coordinator_routes SET cursor=?2 WHERE run=?1",
            params![run, cursor],
        )?;
        tx.commit()?;
    }
    Ok(count)
}

pub fn context(store: &Store, token: &str, run: &str) -> Result<Value> {
    let p = store::authenticate(&store.connection, token)?;
    supervise(&p, run)?;
    let mut snapshot = store.snapshot(token, run)?;
    snapshot["actor_role"] = json!(p.role);
    let mut q=store.connection.prepare("SELECT n.id,n.max_active,(SELECT count(*) FROM attempts a WHERE a.node=n.id AND a.active=1) FROM nodes n WHERE n.active=1 ORDER BY n.id")?;
    let nodes:Vec<Value>=q.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"max_active":r.get::<_,u32>(1)?,"active_attempts":r.get::<_,u32>(2)?})))?.collect::<rusqlite::Result<_>>()?;
    snapshot["nodes"] = json!(nodes);
    snapshot["coordinator_launches"] =
        super::coordinator_launch::summaries(&store.connection, run)?;
    let mut q=store.connection.prepare("SELECT d.task,d.predecessor FROM dependencies d JOIN tasks t ON t.id=d.task WHERE t.run=?1 ORDER BY d.task,d.predecessor")?;
    let dependencies: Vec<Value> = q
        .query_map([run], |r| {
            Ok(json!({"task":r.get::<_,String>(0)?,"predecessor":r.get::<_,String>(1)?}))
        })?
        .collect::<rusqlite::Result<_>>()?;
    snapshot["dependencies"] = json!(dependencies);
    let binding: Option<(u64, String)> = store
        .connection
        .query_row(
            "SELECT generation,endpoint FROM coordinator_routes WHERE run=?1 AND active=1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    snapshot["native_coordinator"]=binding.map(|(generation,endpoint)|Ok::<_,anyhow::Error>(json!({"generation":generation,"endpoint":serde_json::from_str::<Value>(&endpoint)?}))).transpose()?.unwrap_or(Value::Null);
    // Drain the oldest unacknowledged notices first, so a busy run cannot hide
    // outstanding work behind a newer page of already-acknowledged events.
    let mut q=store.connection.prepare("SELECT n.id,n.kind,n.aggregate_id,n.state,n.receipt,n.acknowledged_by FROM coordinator_notices n JOIN coordinator_routes r ON r.run=n.run AND r.generation=n.generation WHERE n.run=?1 AND r.active=1 ORDER BY (n.acknowledged_by IS NOT NULL),n.rowid LIMIT 100")?;
    let mut notices:Vec<Value>=q.query_map([run],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"reference":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"mailer_receipt":r.get::<_,Option<String>>(4)?,"acknowledged_by":r.get::<_,Option<String>>(5)?})))?.collect::<rusqlite::Result<_>>()?;
    for notice in &mut notices {
        if let Some(receipt) = notice["mailer_receipt"].as_str() {
            notice["mailer_receipt"] = serde_json::from_str(receipt)?;
        }
    }
    let remaining: u64 = store.connection.query_row("SELECT count(*) FROM coordinator_notices n JOIN coordinator_routes r ON r.run=n.run AND r.generation=n.generation WHERE n.run=?1 AND r.active=1 AND n.acknowledged_by IS NULL", [run], |r| r.get(0))?;
    snapshot["unacknowledged_notices"] = json!(remaining);
    snapshot["notice_limit"] = json!(100);
    let mut q = store.connection.prepare(
        "SELECT DISTINCT topic FROM memory_revisions WHERE run=?1 ORDER BY topic LIMIT 128",
    )?;
    let topics: Vec<String> = q
        .query_map([run], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    snapshot["memory"] = store.memory(token, run, &topics, 32000)?;
    let mut q=store.connection.prepare("SELECT id,topic,author,source_attempt,text,evidence FROM memory_proposals WHERE run=?1 AND status='proposed' ORDER BY rowid LIMIT 10")?;
    let mut proposals:Vec<Value>=q.query_map([run],|r|Ok(json!({"id":r.get::<_,String>(0)?,"topic":r.get::<_,String>(1)?,"author":r.get::<_,String>(2)?,"source_attempt":r.get::<_,Option<String>>(3)?,"text":r.get::<_,String>(4)?,"evidence":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<_>>()?;
    for proposal in &mut proposals {
        proposal["evidence"] =
            serde_json::from_str(proposal["evidence"].as_str().context("memory evidence")?)?;
    }
    snapshot["memory_proposals"] = json!(proposals);
    snapshot["memory_proposal_limit"] = json!(10);
    let mut q=store.connection.prepare("SELECT topic,MAX(revision) FROM memory_revisions WHERE run=?1 GROUP BY topic ORDER BY topic")?;
    let index: Vec<Value> = q
        .query_map([run], |r| {
            Ok(json!({"topic":r.get::<_,String>(0)?,"revision":r.get::<_,u64>(1)?}))
        })?
        .collect::<rusqlite::Result<_>>()?;
    snapshot["memory_topics"] = json!(index);
    snapshot["notices"] = json!(notices);
    snapshot["instructions"] = json!(include_str!("../../skill/MANAGED_COORDINATOR.md"));
    snapshot["generation"] = json!(p.generation);
    Ok(snapshot)
}

pub fn deliver(store: &mut Store, config: &Config) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "coordinator-notify.lock")?;
    collect(store)?;
    let ids: Vec<String> = {
        let mut q=store.connection.prepare("SELECT id FROM coordinator_notices WHERE state IN ('pending','retry','sending') AND available_at<=?1 ORDER BY rowid LIMIT 32")?;
        q.query_map([now()], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if ids.is_empty() {
        return Ok(json!({"mailer_stored":0,"unresolved":0,"model_processed":false}));
    }
    let token = super::runtime::credential(&config.sender_credential)?;
    let socket = config.mailer_state.join("daemon.sock");
    let caps = super::api::call_json(&socket, &json!({"op":"capabilities","token":token}))?;
    ensure!(
        caps["idempotentSend"] == true && caps["recipientPrecondition"] == true,
        "mailer protocol 2 required"
    );
    let sender = super::api::call_json(&socket, &json!({"op":"doctor","token":token}))?;
    let owner = sender["owner"]
        .as_str()
        .context("mailer sender owner missing")?;
    let author = sender["agent"]
        .as_str()
        .context("mailer sender agent missing")?;
    ensure!(
        !owner.is_empty() && owner.len() <= 256,
        "invalid mailer sender owner"
    );
    identifier(author)?;
    let origin = format!("{owner}/{author}");
    let mut sent = 0;
    let mut unresolved = 0;
    for id in ids {
        let tx = store
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<PendingNotice> = tx
            .query_row(
                "SELECT run,generation,endpoint,body,tries,prepared_body,state,sender_origin FROM coordinator_notices WHERE id=?1 AND state IN ('pending','retry','sending')",
                [&id],
                |r| Ok(PendingNotice {run:r.get(0)?, generation:r.get(1)?, endpoint:r.get(2)?, body:r.get(3)?, tries:r.get(4)?, prepared:r.get(5)?, old_state:r.get(6)?, sender_origin:r.get(7)?}),
            ).optional()?;
        let Some(PendingNotice {
            run,
            generation,
            endpoint,
            body,
            tries,
            prepared,
            old_state,
            sender_origin,
        }) = row
        else {
            continue;
        };
        let current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_routes WHERE run=?1 AND generation=?2 AND active=1)",params![run,generation],|r|r.get(0))?;
        if !current {
            tx.execute(
                "UPDATE coordinator_notices SET state=?2 WHERE id=?1",
                params![
                    id,
                    if old_state == "sending" {
                        "retired_uncertain"
                    } else {
                        "superseded"
                    }
                ],
            )?;
            tx.commit()?;
            continue;
        }
        let endpoint: Endpoint = serde_json::from_str(&endpoint)?;
        if sender_origin.as_ref().is_some_and(|old| old != &origin) {
            tx.execute("UPDATE coordinator_notices SET state='retry',available_at=?2,last_error='mailer sender origin changed; restore original sender before retry' WHERE id=?1",params![id,now()+256])?;
            tx.commit()?;
            unresolved += 1;
            continue;
        }
        let prefix = format!(
            "{} control --state {} context --credential {} {}",
            crate::remote::quote(config.supervisor_binary.to_str().context("binary UTF8")?),
            crate::remote::quote(
                std::fs::canonicalize(&store.dir)?
                    .to_str()
                    .context("state UTF8")?
            ),
            crate::remote::quote(
                credential_path(&std::fs::canonicalize(&store.dir)?, &run, generation)
                    .to_str()
                    .context("credential UTF8")?
            ),
            crate::remote::quote(&run)
        );
        let body = prepared.unwrap_or_else(|| format!("{body}\nContext command: {prefix}"));
        tx.execute("UPDATE coordinator_notices SET state='sending',tries=tries+1,prepared_body=?2,sender_origin=?3 WHERE id=?1",params![id,body,origin])?;
        tx.commit()?;
        let delivered = super::api::call_json(
            &socket,
            &json!({"op":"send","token":token,"recipient":endpoint.agent,"body":body,"operationId":format!("coordinator-{id}"),"recipientSession":endpoint.mailer_session,"recipientEpoch":endpoint.mailer_epoch}),
        ).and_then(|receipt| {
                uuid::Uuid::parse_str(
                    receipt["id"]
                        .as_str()
                        .context("mailer message ID missing")?,
                )?;
                ensure!(
                    receipt["owner"] == owner
                        && receipt["author"] == author
                        && receipt["recipient"] == endpoint.agent
                        && receipt["recipientSession"] == endpoint.mailer_session
                        && receipt["recipientEpoch"] == endpoint.mailer_epoch,
                    "mailer recipient receipt differs"
                );
                Ok(receipt)
        });
        match delivered {
            Ok(receipt) => {
                store.connection.execute("UPDATE coordinator_notices SET receipt=?2,state=CASE WHEN acknowledged_by IS NOT NULL THEN 'acknowledged' WHEN EXISTS(SELECT 1 FROM coordinator_routes r WHERE r.run=coordinator_notices.run AND r.generation=coordinator_notices.generation AND r.active=1) THEN 'mailer_stored' ELSE 'retired_mailer_stored' END,last_error='' WHERE id=?1",params![id,serde_json::to_string(&receipt)?])?;
                sent += 1;
            }
            Err(error) => {
                let delay = 2i64.pow((tries + 1).min(8));
                store.connection.execute("UPDATE coordinator_notices SET state=CASE WHEN acknowledged_by IS NULL THEN 'retry' ELSE 'acknowledged' END,available_at=?2,last_error=?3 WHERE id=?1",params![id,now()+delay,error.to_string().replace(&token,"[redacted]")])?;
                unresolved += 1;
            }
        }
    }
    Ok(json!({"mailer_stored":sent,"unresolved":unresolved,"model_processed":false}))
}
