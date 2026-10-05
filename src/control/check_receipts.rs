//! Independent durable completion evidence for a finished controller check.
//! A missing receipt never permits rerunning a started, potentially live check.
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use super::{
    identity::{Identity, Record, SignedRecord},
    model::{digest, id},
    store::{Store, now, private_dir, secret_file},
};

pub(super) const SCHEME: &str = "local.herdr.supervisor.checkFinish.v1";
const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    scheme: String,
    owner: String,
    initial_hash: String,
    state: String,
    receipt: Value,
    attestation: SignedRecord,
}

fn finish_hash(owner: &str, initial_hash: &str, state: &str, receipt: &Value) -> Result<String> {
    Ok(digest(&serde_json::to_vec(
        &json!({"scheme":SCHEME,"owner":owner,"initial_hash":initial_hash,"state":state,"receipt":receipt}),
    )?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    stdout: String,
    stderr: String,
    stdout_hash: String,
    stderr_hash: String,
    truncated: bool,
    exit_code: Option<i32>,
    timed_out: bool,
}

pub(super) fn path(state: &Path, check: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(check)?;
    Ok(state.join("check-receipts").join(format!("{check}.json")))
}

pub(super) fn prepare(state: &Path) -> Result<()> {
    let dir = state.join("check-receipts");
    private_dir(&dir)?;
    super::artifact_inputs::root(&dir)?;
    std::fs::File::open(state)?.sync_all()?;
    Ok(())
}

fn read(state: &Path, check: &str) -> Result<Option<Finish>> {
    let path = path(state, check)?;
    // Legacy state may have no completion directory. Never create it to infer
    // success, and never follow a replaced directory or receipt symlink.
    match std::fs::symlink_metadata(path.parent().unwrap()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => {
            other?;
        }
    }
    super::artifact_inputs::root(path.parent().unwrap())?;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.permissions().mode() & 0o077 == 0 && m.len() <= MAX_BYTES,
        "completion receipt must be a bounded private regular file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "completion receipt exceeds byte limit"
    );
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn validate(finish: &Finish, initial: &Value, owner: &str) -> Result<()> {
    ensure!(
        initial["receipt_scheme"] == SCHEME
            && finish.scheme == SCHEME
            && finish.owner == owner
            && finish.initial_hash == digest(&serde_json::to_vec(initial)?),
        "completion receipt scope or initial record changed"
    );
    let mut original = finish.receipt.clone();
    let doc = original
        .as_object_mut()
        .context("completion receipt document")?;
    ensure!(
        doc.remove("output").is_some() && doc.remove("finished_at").is_some(),
        "incomplete check outcome"
    );
    doc.remove("checkout_error");
    ensure!(
        &original == initial,
        "completion receipt changed immutable check provenance"
    );
    let started = initial["started_at"].as_i64().context("check start time")?;
    let finished = finish.receipt["finished_at"]
        .as_i64()
        .context("check finish time")?;
    ensure!(
        finished >= started && finished <= now().saturating_add(30),
        "invalid check finish time"
    );
    finish.attestation.verify(now())?;
    let attestation = &finish.attestation.record;
    ensure!(
        attestation.record_type == SCHEME
            && attestation.owner == owner
            && attestation.author == owner
            && attestation.recipient == owner
            && attestation.epoch == 1
            && attestation.id == initial["id"]
            && attestation.created_at == finished
            && attestation.expires_at == i64::MAX
            && attestation.payload
                == json!({"finish_hash":finish_hash(owner,&finish.initial_hash,&finish.state,&finish.receipt)?}),
        "completion attestation does not bind this exact result"
    );
    let output: Output = serde_json::from_value(finish.receipt["output"].clone())?;
    for (excerpt, hash) in [
        (&output.stdout, &output.stdout_hash),
        (&output.stderr, &output.stderr_hash),
    ] {
        ensure!(
            excerpt.chars().count() <= 16384
                && hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid output excerpt or digest"
        );
        if !output.truncated {
            ensure!(
                &digest(excerpt.as_bytes()) == hash,
                "output digest mismatch"
            );
        }
    }
    if let Some(error) = finish.receipt.get("checkout_error") {
        ensure!(
            error.as_str().is_some_and(|s| s.chars().count() <= 4096),
            "invalid checkout observation"
        );
    }
    let passed = output.exit_code == Some(0)
        && !output.timed_out
        && finish.receipt.get("checkout_error").is_none();
    ensure!(
        finish.state == if passed { "passed" } else { "failed" },
        "check outcome contradicts its evidence"
    );
    Ok(())
}

fn apply(store: &mut Store, initial: &Value, finish: &Finish, recovered: bool) -> Result<()> {
    validate(finish, initial, &store.owner()?)?;
    let check = initial["id"].as_str().context("check ID")?;
    uuid::Uuid::parse_str(check)?;
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (integration, name): (String, String) = tx.query_row(
        "SELECT integration,name FROM check_runs WHERE id=?1",
        [check],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let i = super::verification::integration(&tx, &integration)?;
    ensure!(
        initial["integration"] == integration
            && initial["name"] == name
            && initial["candidate"]
                == super::verification::candidate_reference(&i)?.context("check candidate")?
            && initial["policy_hash"] == i.policy_hash,
        "check no longer matches its recorded candidate or policy"
    );
    let n = tx.execute(
        "UPDATE check_runs SET state=?2,doc=?3 WHERE id=?1 AND state='started' AND doc=?4",
        params![
            check,
            finish.state,
            serde_json::to_string(&finish.receipt)?,
            serde_json::to_string(initial)?
        ],
    )?;
    ensure!(
        n == 1,
        "check start record changed before receipt application"
    );
    let audit = json!({"check":check,"integration":integration,"candidate":initial["candidate"],"state":finish.state,"recovered":recovered,"processes_started":false});
    tx.execute("INSERT INTO journal(principal,operation,aggregate_id,kind,doc,created) VALUES ('controller-verifier',?1,?2,'check_receipt_applied',?3,?4)",params![format!("check-finish-{check}"),integration,serde_json::to_string(&audit)?,now()])?;
    tx.commit()?;
    Ok(())
}

/// Caller holds git-executor.lock. Publish atomically without replacing another
/// result, fsync it independently of SQLite, then apply the final SQL receipt.
pub(super) fn finish(
    store: &mut Store,
    initial: &Value,
    receipt: Value,
    passed: bool,
) -> Result<()> {
    let owner = Identity::load(&store.dir.join("owner.key"))?;
    ensure!(
        owner.did() == store.owner()?,
        "completion signing key differs from controller"
    );
    let check = initial["id"].as_str().context("check ID")?;
    let initial_hash = digest(&serde_json::to_vec(initial)?);
    let state = if passed { "passed" } else { "failed" };
    let attestation = owner.sign(Record {
        record_type: SCHEME.into(),
        id: check.into(),
        owner: owner.did(),
        author: owner.did(),
        recipient: owner.did(),
        epoch: 1,
        created_at: receipt["finished_at"]
            .as_i64()
            .context("check finish time")?,
        expires_at: i64::MAX,
        payload: json!({"finish_hash":finish_hash(&owner.did(),&initial_hash,state,&receipt)?}),
    })?;
    let finish = Finish {
        scheme: SCHEME.into(),
        owner: owner.did(),
        initial_hash,
        state: state.into(),
        receipt,
        attestation,
    };
    validate(&finish, initial, &finish.owner)?;
    let check = initial["id"].as_str().context("check ID")?;
    let path = path(&store.dir, check)?;
    let dir = path.parent().unwrap();
    super::artifact_inputs::root(dir)?;
    let bytes = serde_json::to_vec(&finish)?;
    ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "completion receipt exceeds byte limit"
    );
    let temporary = dir.join(format!(".partial-{}", id()));
    let result = (|| -> Result<()> {
        secret_file(&temporary, &bytes)?;
        std::fs::hard_link(&temporary, &path)?;
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result?;
    // No returned error removes the published record. Restart can recover it.
    apply(store, initial, &finish, false)
}

/// Caller holds git-executor.lock; this reads completion evidence only, never
/// launches commands, repairs a checkout or guesses a missing process result.
pub(super) fn recover_one(store: &mut Store, check: &str) -> Result<bool> {
    let initial: Option<String> = store
        .connection
        .query_row(
            "SELECT doc FROM check_runs WHERE id=?1 AND state='started'",
            [check],
            |r| r.get(0),
        )
        .optional()?;
    let Some(initial) = initial else {
        return Ok(false);
    };
    let initial: Value = serde_json::from_str(&initial)?;
    if initial["receipt_scheme"] != SCHEME {
        return Ok(false);
    }
    ensure!(initial["id"] == check, "check receipt ID changed");
    let Some(finish) = read(&store.dir, check)? else {
        return Ok(false);
    };
    apply(store, &initial, &finish, true)?;
    Ok(true)
}

pub(super) fn reconcile(store: &mut Store) -> Result<Value> {
    let _lease = super::api::lock(&store.dir, "git-executor.lock")?;
    let cursor: i64 = store
        .connection
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='check-receipts-cursor'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let rows: Vec<(i64, String)> = {
        let mut q=store.connection.prepare("SELECT rowid,id FROM check_runs WHERE state='started' AND json_extract(doc,'$.receipt_scheme')=?2 ORDER BY (rowid<=?1),rowid LIMIT 16")?;
        q.query_map(params![cursor, SCHEME], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut recovered = Vec::new();
    let mut unresolved = Vec::new();
    for (rowid, check) in rows {
        match recover_one(store,&check) {
            Ok(true)=>recovered.push(check),
            Ok(false)=>unresolved.push(json!({"check":check,"reason":"no durable completion receipt"})),
            Err(_)=>unresolved.push(json!({"check":check,"reason":"completion evidence unavailable, mismatched or not committed"})),
        }
        store.connection.execute("INSERT INTO meta VALUES ('check-receipts-cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[rowid.to_string()])?;
    }
    let (remaining,legacy):(u64,u64)=store.connection.query_row("SELECT count(*),coalesce(sum(json_extract(doc,'$.receipt_scheme') IS NOT ?1),0) FROM check_runs WHERE state='started'",[SCHEME],|r|Ok((r.get(0)?,r.get(1)?)))?;
    Ok(
        json!({"recovered":recovered,"unresolved":unresolved,"limit":16,"remaining_started":remaining,"legacy_started":legacy,"processes_started":false,"resources_deleted":false}),
    )
}
