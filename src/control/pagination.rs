//! Authorized, bounded live scans. Each page has a SQLite read snapshot; a
//! cursor is a position, never a capability or a frozen multi-request snapshot.
use super::{
    model::{Principal, Run},
    store::{self, Store},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PAGE_BYTES: usize = 1024 * 1024;
const ITEM_BYTES: usize = 768 * 1024;
const ENVELOPE_RESERVE: usize = 8192;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Collection {
    Tasks,
    Attempts,
    Results,
    Integrations,
    Checks,
    Reviews,
    Dependencies,
    Nodes,
    Profiles,
    PolicyChecks,
    Memory,
    MemoryProposals,
    PendingMemory,
    Notices,
    PendingNotices,
    CoordinatorLaunches,
}
impl Collection {
    pub const ALL: [Self; 16] = [
        Self::Tasks,
        Self::Attempts,
        Self::Results,
        Self::Integrations,
        Self::Checks,
        Self::Reviews,
        Self::Dependencies,
        Self::Nodes,
        Self::Profiles,
        Self::PolicyChecks,
        Self::Memory,
        Self::MemoryProposals,
        Self::PendingMemory,
        Self::Notices,
        Self::PendingNotices,
        Self::CoordinatorLaunches,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Tasks => "tasks",
            Self::Attempts => "attempts",
            Self::Results => "results",
            Self::Integrations => "integrations",
            Self::Checks => "checks",
            Self::Reviews => "reviews",
            Self::Dependencies => "dependencies",
            Self::Nodes => "nodes",
            Self::Profiles => "profiles",
            Self::PolicyChecks => "policy_checks",
            Self::Memory => "memory",
            Self::MemoryProposals => "memory_proposals",
            Self::PendingMemory => "pending_memory",
            Self::Notices => "notices",
            Self::PendingNotices => "pending_notices",
            Self::CoordinatorLaunches => "coordinator_launches",
        }
    }
    fn policy(self) -> bool {
        matches!(self, Self::Profiles | Self::PolicyChecks)
    }
    fn source(self) -> String {
        match self {
            Self::Tasks => "SELECT t.rowid AS key,json_set(t.doc,'$.dependencies',json((SELECT json_group_array(d.predecessor) FROM dependencies d WHERE d.task=t.id))) AS doc FROM tasks t WHERE t.run=?1".into(),
            Self::Attempts => "SELECT a.rowid AS key,json_set(a.doc,'$.profile_snapshot',json(p.profile)) AS doc FROM attempts a LEFT JOIN attempt_profiles p ON p.attempt=a.id WHERE a.run=?1".into(),
            Self::Results => "SELECT r.rowid AS key,r.doc FROM results r JOIN tasks t ON t.id=r.task WHERE t.run=?1".into(),
            Self::Integrations => "SELECT rowid AS key,doc FROM integrations WHERE run=?1".into(),
            Self::Checks => "SELECT c.rowid AS key,c.doc FROM check_runs c JOIN integrations i ON i.id=c.integration WHERE i.run=?1".into(),
            Self::Reviews => "SELECT v.rowid AS key,v.doc FROM reviews v JOIN tasks t ON t.id=v.task WHERE t.run=?1".into(),
            Self::Dependencies => "SELECT d.rowid AS key,json_object('task',d.task,'predecessor',d.predecessor) AS doc FROM dependencies d JOIN tasks t ON t.id=d.task WHERE t.run=?1".into(),
            Self::Nodes => "SELECT n.rowid AS key,json_object('id',n.id,'max_active',n.max_active,'active_attempts',(SELECT count(*) FROM attempts a WHERE a.node=n.id AND a.active=1)) AS doc FROM nodes n WHERE n.active=1".into(),
            Self::Profiles | Self::PolicyChecks => {
                let field = if self == Self::Profiles {"profiles"} else {"checks"};
                format!("SELECT j.key AS key,json_object('name',j.key,'value',json(j.value)) AS doc FROM runs r,json_each(r.doc,'$.policy.{field}') j WHERE r.id=?1")
            },
            Self::Memory => "SELECT m.rowid AS key,json_object('topic',m.topic,'revision',m.revision,'text',m.text,'proposal',m.proposal,'approved_by',m.approved_by,'withdrawn',json(CASE WHEN m.withdrawn=1 THEN 'true' ELSE 'false' END)) AS doc FROM memory_revisions m WHERE m.run=?1 AND m.revision=(SELECT max(n.revision) FROM memory_revisions n WHERE n.run=m.run AND n.topic=m.topic)".into(),
            Self::MemoryProposals | Self::PendingMemory => {
                let filter = if self==Self::PendingMemory {" AND status='proposed'"} else {""};
                format!("SELECT rowid AS key,json_object('id',id,'topic',topic,'author',author,'source_attempt',source_attempt,'text',text,'evidence',json(evidence),'status',status) AS doc FROM memory_proposals WHERE run=?1{filter}")
            },
            Self::Notices | Self::PendingNotices => {
                let filter=if self==Self::PendingNotices {" AND n.acknowledged_by IS NULL"}else{""};
                // Expose receipt identifiers, never the delivered body or sender capability.
                format!("SELECT n.rowid AS key,json_object('id',n.id,'kind',n.kind,'reference',n.aggregate_id,'state',n.state,'acknowledged_by',n.acknowledged_by,'mailer_receipt_id',json_extract(n.receipt,'$.id')) AS doc FROM coordinator_notices n JOIN coordinator_routes r ON r.run=n.run AND r.generation=n.generation WHERE n.run=?1 AND r.active=1{filter}")
            },
            Self::CoordinatorLaunches => "SELECT l.rowid AS key,json_object('id',l.id,'generation',l.generation,'state',l.state,'profile',json_extract(l.doc,'$.profile_name'),'runtime',json_extract(l.doc,'$.profile.runtime'),'native_session',json_extract(l.doc,'$.expected_session'),'pane',json_extract(l.doc,'$.pane'),'last_error',l.last_error,'process_receipt',json(p.doc),'registration_recovery_count',(SELECT count(*) FROM registration_recoveries rr WHERE rr.target=l.id),'registration_state',CASE WHEN json_extract(l.doc,'$.registration') IS NOT NULL THEN 'registered' WHEN json_extract(l.doc,'$.registration_started') THEN 'uncertain' WHEN EXISTS(SELECT 1 FROM registration_recoveries rr WHERE rr.target=l.id) THEN 'retry_authorized' ELSE 'not_started' END) AS doc FROM coordinator_launches l LEFT JOIN coordinator_processes p ON p.launch=l.id WHERE l.run=?1".into(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    owner: String,
    run: String,
    principal: String,
    collection: Collection,
    after: String,
    through: String,
    policy_revision: Option<u64>,
}
impl Cursor {
    fn encode(&self) -> Result<String> {
        Ok(bs58::encode(serde_json::to_vec(self)?).into_string())
    }
    fn decode(value: &str) -> Result<Self> {
        ensure!(value.len() <= 2048, "page cursor too large");
        let cursor: Self = serde_json::from_slice(&bs58::decode(value).into_vec()?)?;
        ensure!(
            cursor.version == 1 && cursor.after <= cursor.through,
            "invalid page cursor"
        );
        for key in [&cursor.after, &cursor.through] {
            if key.is_empty() {
                continue;
            }
            if cursor.collection.policy() {
                super::model::identifier(key)?;
            } else {
                ensure!(
                    key.len() == 20
                        && key.bytes().all(|b| b.is_ascii_digit())
                        && key.parse::<u64>()? <= i64::MAX as u64,
                    "invalid numeric page cursor"
                );
            }
        }
        Ok(cursor)
    }
}

fn authorize(c: &Connection, token: &str, run: &str) -> Result<(Principal, String, u64)> {
    let p = store::authenticate(c, token)?;
    store::supervise(&p, run)?;
    let revision = c.query_row("SELECT revision FROM runs WHERE id=?1", [run], |r| r.get(0))?;
    let owner = c.query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?;
    Ok((p, owner, revision))
}
fn start(
    c: &Connection,
    owner: &str,
    p: &Principal,
    run: &str,
    collection: Collection,
    revision: u64,
) -> Result<(Cursor, u64)> {
    let (through, count): (String, u64) = c.query_row(
        &format!(
            "SELECT COALESCE(CAST(MAX(key) AS TEXT),''),count(*) FROM ({})",
            collection.source()
        ),
        rusqlite::params_from_iter((!matches!(collection, Collection::Nodes)).then_some(run)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let through = if collection.policy() || through.is_empty() {
        through
    } else {
        format!("{:020}", through.parse::<u64>()?)
    };
    Ok((
        Cursor {
            version: 1,
            owner: owner.into(),
            run: run.into(),
            principal: p.id.clone(),
            collection,
            after: String::new(),
            through,
            policy_revision: collection.policy().then_some(revision),
        },
        count,
    ))
}

pub fn page(
    store: &Store,
    token: &str,
    run: &str,
    collection: Collection,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Value> {
    let tx = store.connection.unchecked_transaction()?;
    let result = read_page(&tx, token, run, collection, cursor, limit)?;
    tx.commit()?;
    Ok(result)
}

fn read_page(
    tx: &Connection,
    token: &str,
    run: &str,
    collection: Collection,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Value> {
    ensure!((1..=100).contains(&limit), "page limit must be 1..100");
    let (p, owner, revision) = authorize(tx, token, run)?;
    let mut position = if let Some(cursor) = cursor {
        Cursor::decode(cursor)?
    } else {
        start(tx, &owner, &p, run, collection, revision)?.0
    };
    ensure!(
        position.owner == owner
            && position.run == run
            && position.principal == p.id
            && position.collection == collection,
        "page cursor belongs to a different scope"
    );
    ensure!(
        position.policy_revision.is_none_or(|old| old == revision),
        "run revision changed; restart this policy collection from a fresh context"
    );
    ensure!(
        position.policy_revision.is_some() == collection.policy(),
        "invalid policy cursor"
    );
    let bounds = if collection.policy() {
        "key>?2 AND key<=?3"
    } else {
        "key>CAST(?2 AS INTEGER) AND key<=CAST(?3 AS INTEGER)"
    };
    let sql = format!(
        "WITH source AS ({}) SELECT key,length(CAST(doc AS BLOB)),CASE WHEN length(CAST(doc AS BLOB))<=?5 THEN doc ELSE NULL END FROM source WHERE {bounds} ORDER BY key LIMIT ?4",
        collection.source()
    );
    let mut q = tx.prepare(&sql)?;
    let mut rows = q.query(params![
        run,
        position.after,
        position.through,
        limit + 1,
        ITEM_BYTES
    ])?;
    let page_cursor = position.encode()?;
    let mut items = Vec::new();
    let mut used = ENVELOPE_RESERVE;
    let mut more = false;
    while let Some(row) = rows.next()? {
        if items.len() >= limit as usize {
            more = true;
            break;
        }
        let key: String = if collection.policy() {
            row.get(0)?
        } else {
            format!("{:020}", row.get::<_, u64>(0)?)
        };
        let size: u64 = row.get(1)?;
        ensure!(
            size <= ITEM_BYTES as u64,
            "record {key} exceeds the {ITEM_BYTES}-byte page item limit; no partial document returned"
        );
        let doc: String = row.get(2)?;
        let item = json!({"key":key,"value":serde_json::from_str::<Value>(&doc)?});
        let size = serde_json::to_vec(&item)?.len() + 1;
        if used + size > PAGE_BYTES {
            more = true;
            break;
        }
        used += size;
        position.after = key;
        items.push(item);
    }
    ensure!(!more || !items.is_empty(), "page cannot make progress");
    let result = json!({"run":run,"collection":collection,"cursor":page_cursor,"items":items,"next_cursor":if more{Some(position.encode()?)}else{None},"consistency":"current_per_page","new_rows":"refresh_context_to_start_a_new_scan","run_revision":revision,"limit":limit,"byte_limit":PAGE_BYTES});
    ensure!(
        serde_json::to_vec(&result)?.len() <= PAGE_BYTES,
        "page envelope too large"
    );
    drop(rows);
    drop(q);
    Ok(result)
}

pub fn overview(store: &Store, token: &str, run: &str) -> Result<Value> {
    let tx = store.connection.unchecked_transaction()?;
    let result = read_overview(&tx, token, run)?;
    tx.commit()?;
    Ok(result)
}

fn read_overview(tx: &Connection, token: &str, run: &str) -> Result<Value> {
    let (p, owner, revision) = authorize(tx, token, run)?;
    // Policy maps have their own pages, so repeated profile edits cannot inflate
    // the overview. Do not deserialize the complete run just to count records.
    let (bytes,doc):(u64,Option<String>)=tx.query_row("SELECT length(CAST(json_remove(doc,'$.policy.profiles','$.policy.checks') AS BLOB)),CASE WHEN length(CAST(json_remove(doc,'$.policy.profiles','$.policy.checks') AS BLOB))<=?2 THEN json_remove(doc,'$.policy.profiles','$.policy.checks') ELSE NULL END FROM runs WHERE id=?1",params![run,ITEM_BYTES],|r|Ok((r.get(0)?,r.get(1)?)))?;
    ensure!(
        bytes <= ITEM_BYTES as u64,
        "run header exceeds context budget"
    );
    let header: Value = serde_json::from_str(&doc.context("run header")?)?;
    let mut collections = serde_json::Map::new();
    for collection in Collection::ALL {
        let (cursor, count) = start(tx, &owner, &p, run, collection, revision)?;
        collections.insert(collection.name().into(),json!({"count":count,"cursor":if count>0{Some(cursor.encode()?)}else{None},"default_limit":50,"byte_limit":PAGE_BYTES}));
    }
    let active: u64 = tx.query_row(
        "SELECT count(*) FROM attempts WHERE run=?1 AND active=1",
        [run],
        |r| r.get(0),
    )?;
    let binding: Option<(u64, String)> = tx
        .query_row(
            "SELECT generation,endpoint FROM coordinator_routes WHERE run=?1 AND active=1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let mut for_recovery = header.clone();
    for_recovery["policy"]["profiles"] = json!({});
    for_recovery["policy"]["checks"] = json!({});
    let minimal: Run = serde_json::from_value(for_recovery)?;
    let result = json!({"kind":"paginated_context","run":header,"actor_role":p.role,"generation":p.generation,"active_attempts":active,"unacknowledged_notices":collections["pending_notices"]["count"],"native_coordinator":binding.map(|(generation,endpoint)|Ok::<_,anyhow::Error>(json!({"generation":generation,"endpoint":serde_json::from_str::<Value>(&endpoint)?}))).transpose()?,"repository_recovery":super::recovery::status(tx,&minimal)?,"collections":collections,"instructions":include_str!("../../skill/MANAGED_COORDINATOR.md"),"consistency":"current_per_page","policy_maps":"Read profiles and policy_checks pages before scheduling or choosing checks"});
    ensure!(
        serde_json::to_vec(&result)?.len() <= PAGE_BYTES,
        "context overview too large"
    );
    Ok(result)
}

/// Bounded panel projection, header and records from the same read transaction.
/// Ordinary cursors remain live scans, not immutable multi-page snapshots.
pub fn project_page(
    store: &Store,
    token: &str,
    run: &str,
    collection: Collection,
    cursor: Option<&str>,
) -> Result<Value> {
    use Collection::*;
    let tx = store.connection.unchecked_transaction()?;
    let mut context = read_overview(&tx, token, run)?;
    let mut page = read_page(&tx, token, run, collection, cursor, 50)?;
    let values: Vec<Value> = page["items"]
        .as_array_mut()
        .context("page items")?
        .drain(..)
        .map(|mut item| item["value"].take())
        .collect();
    page.as_object_mut().unwrap().remove("items");
    page["shown"] = json!(values.len());
    context["kind"] = json!("project_page");
    match collection {
        Tasks => {
            let checks:String = tx.query_row("SELECT json_group_array(j.key) FROM runs r,json_each(r.doc,'$.policy.checks') j WHERE r.id=?1",[run],|r|r.get(0))?;
            ensure!(
                checks.len() <= ITEM_BYTES,
                "authorized check index exceeds form budget"
            );
            context["checks_allowed"] = serde_json::from_str(&checks)?;
            context["task_index_complete"] = json!(false);
            context["tasks"] = json!(values);
        }
        Profiles => {
            let mut profiles = serde_json::Map::new();
            for mut value in values {
                profiles.insert(
                    value["name"].as_str().context("profile name")?.to_string(),
                    value["value"].take(),
                );
            }
            context["profiles"] = json!(profiles);
        }
        PolicyChecks | Nodes | CoordinatorLaunches | Dependencies | Results | Integrations
        | Checks | Reviews => {
            context[collection.name()] = json!(values);
        }
        Attempts => context["attempts"] = json!(values),
        Notices | PendingNotices => context["notices"] = json!(values),
        Memory => {
            let mut topics = serde_json::Map::new();
            let mut withdrawn = Vec::new();
            for value in values {
                if value["withdrawn"] == true {
                    withdrawn.push(value);
                } else {
                    topics.insert(
                        value["topic"].as_str().context("memory topic")?.to_string(),
                        value,
                    );
                }
            }
            context["memory"] = json!({"topics":topics});
            context["withdrawn_memory"] = json!(withdrawn);
        }
        PendingMemory | MemoryProposals => {
            let mut revisions = Vec::new();
            for proposal in &values {
                let topic = proposal["topic"].as_str().context("proposal topic")?;
                let revision: u64 = tx.query_row("SELECT COALESCE(MAX(revision),0) FROM memory_revisions WHERE run=?1 AND topic=?2",params![run,topic],|r|r.get(0))?;
                revisions.push(json!({"topic":topic,"revision":revision}));
            }
            context["memory_topics"] = json!(revisions);
            context["memory_proposals"] = json!(values);
        }
    }
    context["_page"] = page;
    ensure!(
        serde_json::to_vec(&context)?.len() <= 3 * PAGE_BYTES,
        "project page envelope too large"
    );
    tx.commit()?;
    Ok(context)
}

/// Only the facts needed to open/focus a coordinator, independent of task and
/// profile history size. Reservation still rechecks its captured revision.
pub fn open_context(
    store: &Store,
    token: &str,
    run: &str,
    requested_profile: Option<&str>,
) -> Result<Value> {
    if let Some(name) = requested_profile {
        super::model::identifier(name)?;
    }
    let tx = store.connection.unchecked_transaction()?;
    let (principal, _, revision) = authorize(&tx, token, run)?;
    let (project, state): (String, String) =
        tx.query_row("SELECT project,state FROM runs WHERE id=?1", [run], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    let binding: Option<String> = tx
        .query_row(
            "SELECT endpoint FROM coordinator_routes WHERE run=?1 AND active=1",
            [run],
            |r| r.get(0),
        )
        .optional()?;
    let native = binding
        .map(|doc| serde_json::from_str::<Value>(&doc))
        .transpose()?;
    let pending:Option<(String,String,u64,Option<String>)>=tx.query_row("SELECT id,state,length(CAST(last_error AS BLOB)),CASE WHEN length(CAST(last_error AS BLOB))<=16384 THEN last_error ELSE NULL END FROM coordinator_launches WHERE run=?1 AND state<>'exited' ORDER BY generation LIMIT 1",[run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (count,only):(u64,Option<String>)=tx.query_row("SELECT count(*),min(j.key) FROM runs r,json_each(r.doc,'$.policy.profiles') j WHERE r.id=?1",[run],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let selected = if let Some(name) = requested_profile {
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs r,json_each(r.doc,'$.policy.profiles') j WHERE r.id=?1 AND j.key=?2)",params![run,name],|r|r.get(0))?;
        exists.then(|| name.to_string())
    } else if count == 1 {
        only
    } else {
        None
    };
    let result = json!({"kind":"coordinator_open_context","run":{"id":run,"project":project,"state":state,"revision":revision},"actor_role":principal.role,"native_coordinator":native.map(|endpoint|json!({"endpoint":endpoint})),"pending_launch":pending.map(|(id,state,bytes,error)|json!({"id":id,"state":state,"last_error":error,"last_error_bytes":bytes,"last_error_omitted":bytes>16384})),"profile_count":count,"selected_profile":selected});
    ensure!(
        serde_json::to_vec(&result)?.len() <= 262144,
        "coordinator open context too large"
    );
    tx.commit()?;
    Ok(result)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionObservation {
    pub session: String,
    pub runtime: String,
    pub workspace: String,
    pub pane: String,
    pub terminal: String,
}
impl SessionObservation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["codex", "claude", "pi"].contains(&self.runtime.as_str()),
            "unsupported observed runtime"
        );
        for field in [&self.session, &self.workspace, &self.pane, &self.terminal] {
            ensure!(
                !field.is_empty() && field.len() <= 256,
                "invalid observed resource identity"
            );
        }
        Ok(())
    }
}

/// Match observations from a chosen Herdr server without exporting all attempts.
pub fn matches_sessions(
    store: &Store,
    token: &str,
    run: &str,
    observations: &[SessionObservation],
) -> Result<Value> {
    ensure!(
        observations.len() <= 50,
        "at most 50 observations per request"
    );
    let tx = store.connection.unchecked_transaction()?;
    authorize(&tx, token, run)?;
    for observation in observations {
        observation.validate()?;
    }
    let mut matched = false;
    for o in observations {
        matched=tx.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_routes WHERE run=?1 AND active=1 AND json_extract(endpoint,'$.native_session')=?2 AND json_extract(endpoint,'$.runtime')=?3) OR EXISTS(SELECT 1 FROM attempts WHERE run=?1 AND active=1 AND json_extract(doc,'$.native_session')=?2 AND json_extract(doc,'$.resource.runtime')=?3 AND json_extract(doc,'$.resource.workspace')=?4 AND json_extract(doc,'$.resource.pane')=?5 AND json_extract(doc,'$.resource.terminal')=?6)",params![run,o.session,o.runtime,o.workspace,o.pane,o.terminal],|r|r.get(0))?;
        if matched {
            break;
        }
    }
    tx.commit()?;
    Ok(json!({"run":run,"matched":matched}))
}
