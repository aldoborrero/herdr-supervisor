//! Content-bound removal of flat, owned artifact copies after durable intent.
//! Caller holds its execution lease and requires exclusive process quiescence.
use super::{artifact_inputs, artifacts::Candidate};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const MARKER: &str = ".herdr-supervisor-retirement";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}

pub fn identity(path: &Path) -> Result<Identity> {
    artifact_inputs::root(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    Ok(Identity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

fn marker(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join(MARKER))?;
    ensure!(
        file.metadata()?.is_file(),
        "artifact retirement marker is not a regular file"
    );
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024, "artifact retirement marker too large");
    Ok(bytes)
}

fn scan(
    path: &Path,
    candidate: &Candidate,
    expected: &Identity,
    receipt: &[u8],
    partial: bool,
) -> Result<Vec<String>> {
    candidate.validate()?;
    ensure!(
        &identity(path)? == expected,
        "artifact input directory identity changed"
    );
    let mut present = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("unknown artifact input filename"))?;
        if name == MARKER {
            ensure!(
                marker(path)? == receipt,
                "artifact retirement marker changed"
            );
            continue;
        }
        let file = candidate
            .manifest
            .files
            .get(&name)
            .context("unowned file in artifact inputs; retained")?;
        artifact_inputs::read(&path.join(&name), file)?;
        present.push(name);
    }
    ensure!(
        partial || present.len() == candidate.manifest.files.len(),
        "artifact inputs are incomplete before retirement"
    );
    present.sort();
    Ok(present)
}

/// Full inventory before intent: no marker or extra files may be adopted.
pub fn inspect(path: &Path, candidate: &Candidate) -> Result<Identity> {
    let id = identity(path)?;
    ensure!(
        std::fs::symlink_metadata(path.join(MARKER))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
        "unowned retirement marker already exists"
    );
    scan(path, candidate, &id, b"", false)?;
    Ok(id)
}

/// Intent must already be durable. An existing marker is accepted only when its
/// exact bytes match the original decision; it is never replaced or repaired.
pub fn prepare(path: &Path, candidate: &Candidate, id: &Identity, receipt: &[u8]) -> Result<()> {
    scan(path, candidate, id, receipt, false)?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.join(MARKER))
    {
        Ok(mut file) => {
            file.write_all(receipt)?;
            file.sync_all()?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
            marker(path)? == receipt,
            "artifact retirement marker changed"
        ),
        Err(e) => return Err(e.into()),
    }
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub fn remove(path: &Path, candidate: &Candidate, id: &Identity, receipt: &[u8]) -> Result<()> {
    remove_with(path, candidate, id, receipt, |_| Ok(()))
}

pub(super) fn remove_with(
    path: &Path,
    candidate: &Candidate,
    id: &Identity,
    receipt: &[u8],
    mut after_unlink: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let parent = path.parent().context("artifact input parent missing")?;
    let parent_meta = std::fs::symlink_metadata(parent)?;
    ensure!(
        parent_meta.is_dir()
            && parent_meta.mode() & 0o022 == 0
            && std::fs::canonicalize(parent)? == parent,
        "artifact parent must be a real directory writable only by its owner"
    );
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::File::open(parent)?.sync_all()?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    ensure!(
        marker(path)? == receipt,
        "original retirement marker required; directory retained"
    );
    // Verify every remaining file before unlinking any: extra/changed/symlinked
    // data is preserved. Missing declared files are allowed only in this phase.
    let present = scan(path, candidate, id, receipt, true)?;
    for name in present {
        ensure!(
            &identity(path)? == id && marker(path)? == receipt,
            "artifact input ownership changed"
        );
        artifact_inputs::read(&path.join(&name), &candidate.manifest.files[&name])?;
        std::fs::remove_file(path.join(&name))?;
        std::fs::File::open(path)?.sync_all()?;
        after_unlink(&name)?;
    }
    ensure!(
        scan(path, candidate, id, receipt, true)?.is_empty(),
        "artifact input files reappeared"
    );
    std::fs::remove_file(path.join(MARKER))?;
    std::fs::File::open(path)?.sync_all()?;
    // Never recurse or force. If a crash leaves an empty directory without its
    // marker, retain it on retry; explicit operator removal can resolve absence.
    std::fs::remove_dir(path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}
