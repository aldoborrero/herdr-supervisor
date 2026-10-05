//! Owner-managed future profiles; every reserved attempt retains its assigned profile.
use super::{
    identity::SignedRecord,
    model::{Attempt, Integration, Mutation, Policy, Principal, Profile, Run, digest, identifier},
    store::{document, now, own, revision, save_run},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE attempt_profiles (attempt TEXT PRIMARY KEY REFERENCES attempts(id), profile TEXT NOT NULL) STRICT; CREATE TABLE integration_policies (integration TEXT PRIMARY KEY REFERENCES integrations(id), policy TEXT NOT NULL) STRICT;")?;
    // Recover the original assignment, never infer it from a policy that may
    // later change. Missing old execute orders remain explicitly unavailable.
    let mut query = tx.prepare("SELECT o.id,o.payload,o.wire,a.doc FROM outbox o JOIN attempts a ON a.id=json_extract(o.payload,'$.attempt.id') WHERE o.kind='execute'")?;
    let rows: Vec<(String, String, Option<String>, String)> = query
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (effect, payload, wire, attempt) in rows {
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        let a: Attempt = serde_json::from_str(&attempt)?;
        let sent: Attempt = serde_json::from_value(payload["attempt"].clone())?;
        ensure!(
            sent.id == a.id
                && sent.run == a.run
                && sent.node == a.node
                && sent.profile == a.profile
                && sent.contract_hash == a.contract_hash,
            "legacy profile assignment scope mismatch"
        );
        if let Some(wire) = wire {
            let signed: SignedRecord = serde_json::from_str(&wire)?;
            // Historical signature check only: this does not renew the record
            // or authorize its delivery at the current time.
            signed.verify(signed.record.created_at)?;
            let owner: String =
                tx.query_row("SELECT value FROM meta WHERE key='owner'", [], |r| r.get(0))?;
            ensure!(
                signed.record.id == effect
                    && signed.record.owner == owner
                    && signed.record.author == owner
                    && signed.record.record_type == "local.herdr.supervisor.command"
                    && signed.record.payload["kind"] == "execute"
                    && signed.record.payload["body"] == payload,
                "legacy signed assignment conflicts with its stored intent"
            );
        }
        let profile: Profile = serde_json::from_value(payload["profile"].clone())?;
        remember(tx, &a.id, &profile)?;
    }
    let mut query = tx.prepare("SELECT doc FROM integrations")?;
    let candidates = query
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for candidate in candidates {
        let candidate: Integration = serde_json::from_str(&candidate)?;
        let run: Run = document(tx, "runs", &candidate.run)?;
        // Old policy mismatches were already invalid. Do not invent evidence
        // to repair them during migration.
        if policy_digest(&run.policy, &candidate.contract_hash)? == candidate.policy_hash {
            remember_policy(tx, &candidate.id, &run.policy)?;
        }
    }
    tx.pragma_update(None, "user_version", 8)?;
    Ok(())
}

pub(super) fn remember(c: &Connection, attempt: &str, profile: &Profile) -> Result<()> {
    super::runtime::validate_args(&profile.runtime, &profile.argv)?;
    ensure!(profile.limit > 0, "assigned profile has zero limit");
    c.execute(
        "INSERT INTO attempt_profiles VALUES (?1,?2)",
        params![attempt, serde_json::to_string(profile)?],
    )?;
    Ok(())
}

pub(super) fn assigned(c: &Connection, attempt: &str) -> Result<Profile> {
    let profile: Option<String> = c
        .query_row(
            "SELECT profile FROM attempt_profiles WHERE attempt=?1",
            [attempt],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(&profile.context(
        "assigned runtime profile unavailable; inspect the original execute order",
    )?)?)
}

pub(super) fn change(
    tx: &Transaction<'_>,
    principal: &Principal,
    command: &Mutation,
) -> Result<serde_json::Value> {
    own(principal)?;
    let (run, expected_revision, name, profile, create_only) = match command {
        Mutation::SetProfile {
            run,
            expected_revision,
            name,
            profile,
        } => (run, expected_revision, name, profile.as_ref(), false),
        Mutation::CreateProfile {
            run,
            expected_revision,
            name,
            profile,
        } => (run, expected_revision, name, Some(profile), true),
        _ => anyhow::bail!("not a profile decision"),
    };
    identifier(name)?;
    let mut run: Run = document(tx, "runs", run)?;
    revision(run.revision, *expected_revision)?;
    ensure!(
        ["active", "paused"].contains(&run.state.as_str()) && run.deadline > now(),
        "run is expired or closed"
    );
    ensure!(
        !create_only || !run.policy.profiles.contains_key(name),
        "profile already exists; open its editor instead"
    );
    if let Some(profile) = profile {
        run.policy.profiles.insert(name.clone(), profile.clone());
    } else {
        ensure!(
            run.policy.profiles.remove(name).is_some(),
            "unknown run profile"
        );
    }
    run.policy.validate()?;
    run.revision += 1;
    save_run(tx, &run)?;
    if create_only {
        Ok(serde_json::json!({"id":run.id,"revision":run.revision,"name":name,"profile":profile}))
    } else {
        Ok(serde_json::to_value(run)?)
    }
}

pub(super) fn policy_digest(policy: &Policy, contract_hash: &str) -> Result<String> {
    Ok(digest(&serde_json::to_vec(
        &serde_json::json!({"policy":policy,"contract":contract_hash}),
    )?))
}

pub(super) fn remember_policy(c: &Connection, integration: &str, policy: &Policy) -> Result<()> {
    c.execute(
        "INSERT INTO integration_policies VALUES (?1,?2)",
        params![integration, serde_json::to_string(policy)?],
    )?;
    Ok(())
}

pub(super) fn validate_policy(
    c: &Connection,
    candidate: &Integration,
    current: &Policy,
) -> Result<()> {
    let policy: Option<String> = c
        .query_row(
            "SELECT policy FROM integration_policies WHERE integration=?1",
            [&candidate.id],
            |r| r.get(0),
        )
        .optional()?;
    let original: Policy = serde_json::from_str(
        &policy
            .context("verification policy snapshot unavailable; candidate remains unresolved")?,
    )?;
    ensure!(
        policy_digest(&original, &candidate.contract_hash)? == candidate.policy_hash,
        "verification policy snapshot changed"
    );
    let mut before = original;
    let mut after = current.clone();
    before.profiles.clear();
    after.profiles.clear();
    ensure!(before == after, "candidate verification policy changed");
    Ok(())
}
