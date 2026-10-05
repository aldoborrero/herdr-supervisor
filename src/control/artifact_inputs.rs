//! Owned artifact copies. Once published ready, inputs are verified, never repaired.
use super::{
    artifacts::{Candidate, File},
    model::{digest, id},
    store::private_dir,
};
use anyhow::{Context, Result, ensure};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn root(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir()
            && meta.permissions().mode() & 0o077 == 0
            && std::fs::canonicalize(path)? == path,
        "artifact input directory was replaced or is not private"
    );
    Ok(())
}
pub(super) fn read(path: &Path, file: &File) -> Result<Vec<u8>> {
    let handle = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = handle.metadata()?;
    ensure!(
        meta.is_file() && meta.len() == file.size,
        "artifact input is not the declared regular file"
    );
    let mut data = Vec::new();
    handle.take(file.size + 1).read_to_end(&mut data)?;
    ensure!(
        data.len() as u64 == file.size && digest(&data) == file.sha256,
        "artifact input content changed"
    );
    Ok(data)
}
pub fn verify(path: &Path, candidate: &Candidate) -> Result<()> {
    candidate.validate()?;
    root(path)?;
    for (name, file) in &candidate.manifest.files {
        read(&path.join(name), file)?;
    }
    Ok(())
}

/// Caller must persist exclusive ownership intent before invoking this function.
/// Each file is installed atomically; interrupted temporary files are never adopted.
pub fn prepare(
    path: &Path,
    candidate: &Candidate,
    mut load: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    candidate.validate()?;
    private_dir(path)?;
    root(path)?;
    std::fs::File::open(path.parent().context("artifact parent")?)?.sync_all()?;
    for (name, file) in &candidate.manifest.files {
        let target = path.join(name);
        if read(&target, file).is_ok() {
            continue;
        }
        let bytes = load(name)?;
        install(path, name, file, &bytes)?;
    }
    verify(path, candidate)
}

pub fn controller(store: &super::store::Store, i: &super::model::Integration) -> Result<PathBuf> {
    use rusqlite::{OptionalExtension, params};
    let candidate = i.artifact.as_ref().context("artifact candidate missing")?;
    let parent = store.dir.join("verification");
    private_dir(&parent)?;
    std::fs::File::open(&store.dir)?.sync_all()?;
    let dir = parent.join(&i.id);
    private_dir(&dir)?;
    std::fs::File::open(&parent)?.sync_all()?;
    let path = dir.join("inputs");
    let prior: Option<(String, String)> = store
        .connection
        .query_row(
            "SELECT path,state FROM verification_worktrees WHERE integration=?1",
            [&i.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let ready = if let Some((old, state)) = prior {
        ensure!(Path::new(&old) == path, "artifact input path changed");
        ensure!(
            ["creating", "ready"].contains(&state.as_str()),
            "unknown artifact preparation state"
        );
        state == "ready"
    } else {
        ensure!(
            !path.try_exists()? && std::fs::symlink_metadata(&path).is_err(),
            "refusing to adopt existing artifact inputs"
        );
        store.connection.execute(
            "INSERT INTO verification_worktrees VALUES (?1,?2,'creating')",
            params![i.id, path.to_string_lossy()],
        )?;
        false
    };
    if ready {
        verify(&path, candidate)?;
    } else {
        prepare(&path, candidate, |name| {
            super::artifacts::content(&store.connection, candidate, name)
        })?;
        store.connection.execute(
            "UPDATE verification_worktrees SET state='ready' WHERE integration=?1",
            [&i.id],
        )?;
    }
    Ok(path)
}

pub(super) fn install(path: &Path, name: &str, file: &File, bytes: &[u8]) -> Result<()> {
    root(path)?;
    super::artifacts::name(name)?;
    let target = path.join(name);
    ensure!(
        bytes.len() as u64 == file.size && digest(bytes) == file.sha256,
        "source artifact content mismatch"
    );
    let temporary = path.join(format!(".partial-{}", id()));
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&temporary)?;
    let outcome = (|| -> Result<()> {
        output.write_all(bytes)?;
        output.sync_all()?;
        std::fs::rename(&temporary, &target)?;
        std::fs::File::open(path)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome?;
    Ok(())
}
