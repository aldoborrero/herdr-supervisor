//! Review inputs arrive through assigned, signed chunk receipts before native bootstrap.
use super::{
    artifacts::{CHUNK_BYTES, Candidate, Chunk},
    identity::Identity,
    model::{Mutation, Request, digest},
    node::{ExecuteOrder, NodeStore},
    store::private_dir,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "CREATE TABLE artifact_staging (
 attempt TEXT PRIMARY KEY REFERENCES jobs(id), epoch INTEGER NOT NULL,
 manifest_hash TEXT NOT NULL, path TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('creating','ready'))
) STRICT; PRAGMA user_version=7;";

fn ready_record(
    store: &NodeStore,
    order: &ExecuteOrder,
    candidate: &Candidate,
) -> Result<Option<(String, String)>> {
    let row: Option<(u64, String, String, String)> = store
        .connection
        .query_row(
            "SELECT epoch,manifest_hash,path,state FROM artifact_staging WHERE attempt=?1",
            [&order.attempt.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(epoch, hash, path, state)| {
        ensure!(
            epoch == order.attempt.epoch && hash == candidate.manifest_hash,
            "artifact staging belongs to another assignment"
        );
        Ok((path, state))
    })
    .transpose()
}

pub fn stage(
    store: &mut NodeStore,
    order: &ExecuteOrder,
    worktree: &Path,
    key: &Identity,
) -> Result<Option<PathBuf>> {
    let review = order.review.as_ref().context("review context missing")?;
    let candidate = review
        .artifact
        .as_ref()
        .context("artifact review context missing")?;
    candidate.validate()?;
    let path = worktree.join(format!(".herdr-supervisor-artifacts-{}", order.attempt.id));
    let ready = if let Some((old, state)) = ready_record(store, order, candidate)? {
        ensure!(Path::new(&old) == path, "artifact review path changed");
        state == "ready"
    } else {
        ensure!(
            !path.try_exists()? && std::fs::symlink_metadata(&path).is_err(),
            "refusing to adopt existing review inputs"
        );
        store.connection.execute(
            "INSERT INTO artifact_staging VALUES (?1,?2,?3,?4,'creating')",
            params![
                order.attempt.id,
                order.attempt.epoch,
                candidate.manifest_hash,
                path.to_string_lossy()
            ],
        )?;
        false
    };
    if ready {
        super::artifact_inputs::verify(&path, candidate)?;
        return Ok(Some(path));
    }
    private_dir(&path)?;
    super::artifact_inputs::root(&path)?;
    std::fs::File::open(worktree)?.sync_all()?;
    let mut queued = 0;
    let mut all_ready = true;
    for (name, file) in &candidate.manifest.files {
        if super::artifact_inputs::read(&path.join(name), file).is_ok() {
            continue;
        }
        let mut bytes = Vec::new();
        let mut complete = true;
        for index in 0..file.size.div_ceil(CHUNK_BYTES as u64).max(1) {
            let operation = digest(&serde_json::to_vec(&(
                "artifact-review",
                &order.attempt.id,
                order.attempt.epoch,
                &review.integration,
                &candidate.manifest_hash,
                name,
                index,
            ))?);
            let event_key = format!("{}:{operation}", digest(order.credential.as_bytes()));
            let prior: Option<(String, Option<String>)> = store
                .connection
                .query_row(
                    "SELECT state,response FROM events WHERE id=?1",
                    [&event_key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((state, Some(response))) = prior
                .as_ref()
                .filter(|(state, _)| state == "controller_applied")
            {
                ensure!(
                    state == "controller_applied",
                    "artifact receipt not applied"
                );
                let response: Value = serde_json::from_str(response)?;
                ensure!(
                    response["id"] == order.attempt.id
                        && response["integration"] == review.integration
                        && response["manifest_hash"] == candidate.manifest_hash,
                    "artifact receipt assignment mismatch"
                );
                let chunk: Chunk = serde_json::from_value(response["chunk"].clone())?;
                chunk.validate()?;
                ensure!(
                    chunk.name == *name
                        && chunk.sha256 == file.sha256
                        && chunk.size == file.size
                        && u64::from(chunk.index) == index,
                    "artifact receipt chunk mismatch"
                );
                bytes.extend(chunk.data);
            } else {
                complete = false;
                if prior.is_none() {
                    if queued == 16 {
                        return Ok(None);
                    }
                    store.enqueue(
                        &Request {
                            token: order.credential.clone(),
                            operation_id: operation,
                            command: Mutation::FetchArtifactChunk {
                                attempt: order.attempt.id.clone(),
                                epoch: order.attempt.epoch,
                                integration: review.integration.clone(),
                                name: name.clone(),
                                index: index as u32,
                            },
                        },
                        key,
                    )?;
                    queued += 1;
                }
            }
        }
        if complete {
            super::artifact_inputs::install(&path, name, file, &bytes)?;
        } else {
            all_ready = false;
        }
    }
    if !all_ready {
        return Ok(None);
    }
    super::artifact_inputs::verify(&path, candidate)?;
    store.connection.execute(
        "UPDATE artifact_staging SET state='ready' WHERE attempt=?1 AND epoch=?2",
        params![order.attempt.id, order.attempt.epoch],
    )?;
    Ok(Some(path))
}

pub fn verify_ready(store: &NodeStore, order: &ExecuteOrder) -> Result<PathBuf> {
    let candidate = order
        .review
        .as_ref()
        .and_then(|r| r.artifact.as_ref())
        .context("artifact review missing")?;
    let (path, state) =
        ready_record(store, order, candidate)?.context("artifact inputs not prepared")?;
    ensure!(state == "ready", "artifact inputs are incomplete");
    let path = PathBuf::from(path);
    super::artifact_inputs::verify(&path, candidate)?;
    Ok(path)
}
