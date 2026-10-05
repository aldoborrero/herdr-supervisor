//! Attempt-scoped artifact bytes. A declared digest is never proof of stored content.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::model::{Attempt, Mutation, Principal, Run, digest};
use super::store::{document, now, worker};

pub const CHUNK_BYTES: usize = 16_384;
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_ATTEMPT_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_FILES: u64 = 32;
pub const SCHEMA: &str = "
CREATE TABLE artifact_uploads (
 attempt TEXT NOT NULL REFERENCES attempts(id), name TEXT NOT NULL,
 sha256 TEXT NOT NULL, size INTEGER NOT NULL, complete INTEGER NOT NULL CHECK(complete IN (0,1)),
 PRIMARY KEY(attempt,name)
) STRICT;
CREATE TABLE artifact_chunks (
 attempt TEXT NOT NULL, name TEXT NOT NULL, chunk_index INTEGER NOT NULL, data BLOB NOT NULL,
 PRIMARY KEY(attempt,name,chunk_index),
 FOREIGN KEY(attempt,name) REFERENCES artifact_uploads(attempt,name)
) STRICT;
PRAGMA user_version=7;";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub name: String,
    pub sha256: String,
    pub size: u64,
    pub index: u32,
    pub data: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub attempt: String,
    pub contract_hash: String,
    pub files: BTreeMap<String, File>,
    pub version: u32,
}

pub fn name(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 100
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "artifact name must be a flat ASCII filename beginning with a letter or digit"
    );
    Ok(())
}
pub fn hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "artifact hash must be lowercase SHA-256"
    );
    Ok(())
}
fn chunks(size: u64) -> u64 {
    size.div_ceil(CHUNK_BYTES as u64).max(1)
}
impl Chunk {
    pub fn validate(&self) -> Result<()> {
        name(&self.name)?;
        hash(&self.sha256)?;
        ensure!(
            self.size <= MAX_FILE_BYTES && u64::from(self.index) < chunks(self.size),
            "artifact size or chunk index exceeds limit"
        );
        let offset = u64::from(self.index) * CHUNK_BYTES as u64;
        let length = (self.size - offset).min(CHUNK_BYTES as u64) as usize;
        ensure!(
            self.data.len() == length,
            "artifact chunk length does not match declared size and index"
        );
        Ok(())
    }
}

pub fn put(
    tx: &Transaction<'_>,
    principal: &Principal,
    attempt: &str,
    epoch: u64,
    chunk: &Chunk,
) -> Result<Value> {
    chunk.validate()?;
    let a: Attempt = document(tx, "attempts", attempt)?;
    worker(principal, &a, epoch)?;
    ensure!(
        a.role == "worker" && a.state == "running",
        "only a ready author can upload artifact content"
    );
    let run: Run = document(tx, "runs", &a.run)?;
    ensure!(
        ["active", "paused"].contains(&run.state.as_str()) && now() < run.deadline,
        "run closed or expired"
    );
    let prior: Option<(String, u64)> = tx
        .query_row(
            "SELECT sha256,size FROM artifact_uploads WHERE attempt=?1 AND name=?2",
            params![attempt, chunk.name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some(prior) = prior {
        ensure!(
            prior == (chunk.sha256.clone(), chunk.size),
            "artifact name already identifies different content"
        );
    } else {
        let (files, total): (u64, u64) = tx.query_row(
            "SELECT count(*),COALESCE(sum(size),0) FROM artifact_uploads WHERE attempt=?1",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            files < MAX_FILES && total + chunk.size <= MAX_ATTEMPT_BYTES,
            "attempt artifact quota exceeded"
        );
        tx.execute(
            "INSERT INTO artifact_uploads VALUES (?1,?2,?3,?4,0)",
            params![attempt, chunk.name, chunk.sha256, chunk.size],
        )?;
    }
    let old: Option<Vec<u8>> = tx
        .query_row(
            "SELECT data FROM artifact_chunks WHERE attempt=?1 AND name=?2 AND chunk_index=?3",
            params![attempt, chunk.name, chunk.index],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        ensure!(old == chunk.data, "artifact chunk content collision");
    } else {
        tx.execute(
            "INSERT INTO artifact_chunks VALUES (?1,?2,?3,?4)",
            params![attempt, chunk.name, chunk.index, chunk.data],
        )?;
    }
    let received: u64 = tx.query_row(
        "SELECT count(*) FROM artifact_chunks WHERE attempt=?1 AND name=?2",
        params![attempt, chunk.name],
        |r| r.get(0),
    )?;
    let complete = received == chunks(chunk.size);
    if complete {
        verify(
            tx,
            attempt,
            &chunk.name,
            &File {
                sha256: chunk.sha256.clone(),
                size: chunk.size,
            },
        )?;
        tx.execute(
            "UPDATE artifact_uploads SET complete=1 WHERE attempt=?1 AND name=?2",
            params![attempt, chunk.name],
        )?;
    }
    Ok(
        json!({"attempt":attempt,"name":chunk.name,"sha256":chunk.sha256,"size":chunk.size,"chunk":chunk.index,"received_chunks":received,"expected_chunks":chunks(chunk.size),"content_verified":complete,"task_accepted":false}),
    )
}

/// Rehash all bounded chunks, including on result sealing; metadata alone is insufficient.
pub fn verify(c: &Connection, attempt: &str, name: &str, file: &File) -> Result<()> {
    ensure!(file.size <= MAX_FILE_BYTES, "artifact size exceeds limit");
    hash(&file.sha256)?;
    let mut q=c.prepare("SELECT chunk_index,data FROM artifact_chunks WHERE attempt=?1 AND name=?2 ORDER BY chunk_index")?;
    let mut rows = q.query(params![attempt, name])?;
    let mut index = 0_u64;
    let mut size = 0_u64;
    let mut hasher = Sha256::new();
    while let Some(row) = rows.next()? {
        let n: u64 = row.get(0)?;
        let data: Vec<u8> = row.get(1)?;
        ensure!(
            n == index && n < chunks(file.size),
            "artifact chunks are missing or unexpected"
        );
        let expected = (file.size - n * CHUNK_BYTES as u64).min(CHUNK_BYTES as u64) as usize;
        ensure!(
            data.len() == expected,
            "stored artifact chunk length mismatch"
        );
        size += data.len() as u64;
        index += 1;
        hasher.update(data);
    }
    ensure!(
        size == file.size
            && index == chunks(file.size)
            && format!("{:x}", hasher.finalize()) == file.sha256,
        "artifact bytes do not match complete declared content"
    );
    Ok(())
}

pub fn seal(
    c: &Connection,
    a: &Attempt,
    selected: &BTreeMap<String, String>,
) -> Result<Option<Manifest>> {
    ensure!(
        selected.len() <= MAX_FILES as usize,
        "artifact manifest too large"
    );
    if selected.is_empty() {
        return Ok(None);
    }
    let mut files = BTreeMap::new();
    for (name, sha256) in selected {
        self::name(name)?;
        hash(sha256)?;
        let stored: Option<(String, u64, bool)> = c
            .query_row(
                "SELECT sha256,size,complete FROM artifact_uploads WHERE attempt=?1 AND name=?2",
                params![a.id, name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (actual, size, complete) =
            stored.context("artifact bytes have not been uploaded for this attempt")?;
        ensure!(
            actual == *sha256 && complete,
            "artifact content is incomplete or differs from result"
        );
        let file = File {
            sha256: actual,
            size,
        };
        verify(c, &a.id, name, &file)?;
        files.insert(name.clone(), file);
    }
    Ok(Some(Manifest {
        attempt: a.id.clone(),
        contract_hash: a.contract_hash.clone(),
        files,
        version: 1,
    }))
}

/// Node status uses only scoped, authoritative controller receipts; queueing is not proof.
pub fn status(
    c: &Connection,
    token: &str,
    attempt: &str,
    name: &str,
    sha256: &str,
) -> Result<Value> {
    self::name(name)?;
    hash(sha256)?;
    let mut q=c.prepare("SELECT state,response,json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.command.params.chunk.index') FROM events WHERE substr(id,1,64)=?1 AND json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.command.method')='put_artifact' AND json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.command.params.attempt')=?2 AND json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.command.params.chunk.name')=?3 AND json_extract(CASE WHEN json_valid(wire) THEN wire ELSE '{}' END,'$.record.payload.command.params.chunk.sha256')=?4")?;
    let mut rows = q.query(params![digest(token.as_bytes()), attempt, name, sha256])?;
    let mut sent = std::collections::BTreeSet::new();
    let mut applied = std::collections::BTreeSet::new();
    let mut terminal = 0_u64;
    let mut verified = false;
    while let Some(row) = rows.next()? {
        let state: String = row.get(0)?;
        let index: u32 = row.get(2)?;
        sent.insert(index);
        if ["expired", "invalid"].contains(&state.as_str()) {
            terminal += 1;
        }
        if state == "controller_applied" {
            let receipt: Value = serde_json::from_str(&row.get::<_, String>(1)?)?;
            ensure!(
                receipt["attempt"] == attempt
                    && receipt["name"] == name
                    && receipt["sha256"] == sha256
                    && receipt["chunk"] == index,
                "artifact receipt scope mismatch"
            );
            applied.insert(index);
            verified |= receipt["content_verified"] == true;
        }
    }
    Ok(
        json!({"attempt":attempt,"name":name,"sha256":sha256,"queued_chunks":sent.len(),"applied_chunks":applied.len(),"terminal_deliveries":terminal,"content_verified":verified,"task_accepted":false}),
    )
}

/// One bounded read freezes a local file before enqueueing deterministic chunk operations.
pub fn upload(
    state: &Path,
    token: &str,
    attempt: &str,
    epoch: u64,
    name: &str,
    file: &Path,
) -> Result<Value> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    self::name(name)?;
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)?;
    let meta = handle.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_FILE_BYTES,
        "artifact must be a regular file of at most 8 MiB"
    );
    let mut bytes = Vec::new();
    handle.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "artifact grew beyond its size limit"
    );
    let sha256 = digest(&bytes);
    let mut events = Vec::new();
    for index in 0..chunks(bytes.len() as u64) {
        let offset = index as usize * CHUNK_BYTES;
        let chunk = Chunk {
            name: name.into(),
            sha256: sha256.clone(),
            size: bytes.len() as u64,
            index: index as u32,
            data: bytes[offset..bytes.len().min(offset + CHUNK_BYTES)].to_vec(),
        };
        let operation_id = digest(&serde_json::to_vec(&(
            attempt,
            epoch,
            &chunk.name,
            &chunk.sha256,
            chunk.size,
            chunk.index,
        ))?);
        let result = enqueue_when_available(
            state,
            &super::worker::WorkerRequest::Apply {
                token: token.into(),
                operation_id,
                command: Box::new(Mutation::PutArtifact {
                    attempt: attempt.into(),
                    epoch,
                    chunk,
                }),
            },
        )?;
        events.push(
            result["event"]
                .as_str()
                .context("artifact event receipt missing")?
                .to_string(),
        );
    }
    Ok(
        json!({"attempt":attempt,"name":name,"sha256":sha256,"size":bytes.len(),"events":events,"content_verified":false,"task_accepted":false}),
    )
}

// Busy execution fences are safe to retry with exactly the same operation ID.
// Other failures (including an ambiguous IPC reply) surface for explicit resume.
fn enqueue_when_available(state: &Path, request: &super::worker::WorkerRequest) -> Result<Value> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match super::worker::call(state, request) {
            Err(error)
                if error.to_string() == "another process owns this state"
                    && std::time::Instant::now() < until =>
            {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            result => return result,
        }
    }
}
