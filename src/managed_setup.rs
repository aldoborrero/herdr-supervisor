//! Retryable onboarding publishes a fully bound view, never a legacy project.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use crate::{
    control::{
        api::{self, ApiRequest},
        model::{Mutation, Policy, Run, digest, id, oid},
        runtime,
        store::private_dir,
    },
    managed::{self, Binding},
    project::{self, Project},
};

const MAX_SPEC: usize = 65536;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub name: String,
    pub controller: PathBuf,
    pub credential: PathBuf,
    pub repo: String,
    pub base_oid: String,
    pub objective: String,
    pub policy: Policy,
    #[serde(default)]
    pub launch_config: Option<PathBuf>,
}

impl Spec {
    pub(crate) fn validate(&self) -> Result<()> {
        project::slug_from_name(&self.name)?;
        ensure!(self.name.len() <= 512, "project name too long");
        ensure!(
            Path::new(&self.repo).is_absolute() && self.repo.len() <= 4096,
            "absolute controller repository ID required"
        );
        ensure!(
            !self.objective.trim().is_empty() && self.objective.len() <= 16384,
            "objective must contain 1..16384 bytes of text"
        );
        oid(&self.base_oid)?;
        self.policy.validate()?;
        ensure!(
            serde_json::to_vec(self)?.len() <= MAX_SPEC,
            "setup specification too large"
        );
        Ok(())
    }

    fn normalize(mut self) -> Result<Self> {
        self.validate()?;
        self.controller = std::fs::canonicalize(self.controller)?;
        self.credential = std::fs::canonicalize(self.credential)?;
        self.launch_config = self.launch_config.map(std::fs::canonicalize).transpose()?;
        if let Some(path) = &self.launch_config {
            crate::control::coordinator_launch::read_config(path)?;
        }
        ensure!(
            serde_json::to_vec(&self)?.len() <= MAX_SPEC,
            "setup specification too large"
        );
        Ok(self)
    }
}

pub fn parse(json: &str) -> Result<Spec> {
    ensure!(json.len() <= MAX_SPEC, "setup specification too large");
    Ok(serde_json::from_str(json)?)
}

fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "setup record must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "setup record too large");
    Ok(bytes)
}

pub fn read_spec(path: &Path) -> Result<Spec> {
    // Explicit read-only configuration can be a Home Manager/Nix store symlink.
    // Mutable intent/origin records below still reject symlink replacement.
    Ok(serde_json::from_slice(&read_bytes(
        &std::fs::canonicalize(path)?,
        MAX_SPEC,
    )?)?)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    operation: String,
    spec: Spec,
    spec_hash: String,
    credential_hash: String,
    run: Option<String>,
    published: bool,
}

#[derive(Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Origin {
    operation: String,
    spec_hash: String,
    run: String,
}

fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sync_tree(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            ensure!(
                kind.is_file(),
                "unexpected link or special file in staged project"
            );
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

fn published(root: &Path, slug: &str, origin: &Origin, binding: &Binding) -> Result<bool> {
    let path = root.join(slug);
    if !exists(&path)? {
        return Ok(false);
    }
    ensure!(
        std::fs::symlink_metadata(&path)?.is_dir(),
        "project destination is not a real directory"
    );
    let project = Project::load_view(root, slug)?;
    let recorded: Origin = serde_json::from_slice(&read_bytes(
        &project.state_dir().join("managed-origin.json"),
        MAX_SPEC,
    )?)?;
    ensure!(
        &recorded == origin && managed::binding(&project)? == *binding,
        "project destination belongs to another setup; existing content is preserved"
    );
    Ok(true)
}

pub fn create(root: &Path, spec: Spec) -> Result<Value> {
    let slug = project::slug_from_name(&spec.name)?;
    let result = create_with(root, spec, api::call, |_, _, _| Ok(()));
    if root
        .join(".managed-setup")
        .join(&slug)
        .join("intent.json")
        .exists()
    {
        result.with_context(|| {
            format!("Setup retained; retry the same inputs or use managed resume-setup {slug}")
        })
    } else {
        result
    }
}

fn saved(root: &Path, slug: &str) -> Result<Intent> {
    project::validate_slug(slug)?;
    let intent: Intent = serde_json::from_slice(&read_bytes(
        &root.join(".managed-setup").join(slug).join("intent.json"),
        MAX_SPEC * 2,
    )?)?;
    ensure!(
        intent.version == 1 && project::slug_from_name(&intent.spec.name)? == slug,
        "saved setup scope mismatch"
    );
    Ok(intent)
}

pub fn resume(root: &Path, slug: &str) -> Result<Value> {
    create(root, saved(root, slug)?.spec)
}

pub fn status(root: &Path, slug: &str) -> Result<Value> {
    let intent = saved(root, slug)?;
    Ok(
        json!({"project":slug,"operation":intent.operation,"run":intent.run,
        "recorded_state":if intent.published { "published" } else if intent.run.is_some() { "publication_unconfirmed" } else { "controller_unconfirmed" },
        "controller":intent.spec.controller,"current_controller_state_verified":false}),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum Publication {
    Before,
    After,
}

// The call boundary allows a lost controller response to be tested without
// bypassing authority handlers. Publication hooks inject races and lost receipts.
fn create_with(
    root: &Path,
    spec: Spec,
    mut call: impl FnMut(&Path, &ApiRequest) -> Result<Value>,
    mut publication: impl FnMut(Publication, &Path, &Path) -> Result<()>,
) -> Result<Value> {
    let spec = spec.normalize()?;
    let slug = project::slug_from_name(&spec.name)?;
    let credential = runtime::credential(&spec.credential)?;
    let credential_hash = digest(credential.as_bytes());
    let spec_hash = digest(&serde_json::to_vec(&spec)?);
    std::fs::create_dir_all(root)?;
    let root = std::fs::canonicalize(root)?;
    let setups = root.join(".managed-setup");
    private_dir(&setups)?;
    File::open(&root)?.sync_all()?;
    let state = setups.join(&slug);
    private_dir(&state)?;
    File::open(&setups)?.sync_all()?;
    let _lease = api::lock(&state, "setup.lock")?;
    let record = state.join("intent.json");
    let mut intent = if exists(&record)? {
        let old: Intent = serde_json::from_slice(&read_bytes(&record, MAX_SPEC * 2)?)?;
        ensure!(
            old.version == 1
                && old.spec == spec
                && old.spec_hash == spec_hash
                && old.credential_hash == credential_hash,
            "setup inputs or owner credential changed; original operation retained"
        );
        crate::control::model::identifier(&old.operation)?;
        old
    } else {
        ensure!(
            !exists(&root.join(&slug))?,
            "project destination already exists; use managed attach for an existing empty project"
        );
        let intent = Intent {
            version: 1,
            operation: id(),
            spec,
            spec_hash,
            credential_hash,
            run: None,
            published: false,
        };
        project::write_json(&record, &intent)?;
        intent
    };
    // Persisted before IPC. Every retry uses the identical owner capability and
    // operation contents, including after a response was accepted but lost.
    let spec = &intent.spec;
    let response = call(
        &spec.controller,
        &ApiRequest::Apply {
            token: credential,
            operation_id: intent.operation.clone(),
            command: Box::new(Mutation::CreateRun {
                project: slug.clone(),
                repo: spec.repo.clone(),
                base_oid: spec.base_oid.clone(),
                objective: spec.objective.clone(),
                policy: spec.policy.clone(),
            }),
        },
    )?;
    let run: Run = serde_json::from_value(response)?;
    ensure!(
        run.project == slug
            && run.repo == spec.repo
            && run.base_oid == spec.base_oid
            && run.objective == spec.objective
            && run.policy == spec.policy
            && intent.run.as_ref().is_none_or(|id| id == &run.id),
        "controller setup receipt scope mismatch"
    );
    crate::control::model::identifier(&run.id)?;
    intent.run = Some(run.id.clone());
    project::write_json(&record, &intent)?;
    let binding = Binding {
        version: 1,
        controller: intent.spec.controller.clone(),
        credential: intent.spec.credential.clone(),
        run: run.id.clone(),
        launch_config: intent.spec.launch_config.clone(),
    };
    let origin = Origin {
        operation: intent.operation.clone(),
        spec_hash: intent.spec_hash.clone(),
        run: run.id.clone(),
    };
    if !published(&root, &slug, &origin, &binding)? {
        ensure!(
            !intent.published,
            "previously published project is missing; automatic replacement refused"
        );
        // A failed/unpublished staging directory is private and retained. A new
        // staging directory cannot start agents or become a second project view.
        let staging = state.join(format!("staging-{}", id()));
        private_dir(&staging)?;
        File::open(&state)?.sync_all()?;
        let project = project::create(&staging, &intent.spec.name, &intent.spec.objective, vec![])?;
        drop(project);
        let project = Project::load_unlocked(&staging, &slug)?;
        let _authority = managed::authority_lease(&project, true)?;
        managed::write_binding(&project, &binding)?;
        project::write_json(&project.state_dir().join("managed-origin.json"), &origin)?;
        sync_tree(&project.dir())?;
        File::open(&staging)?.sync_all()?;
        publication(Publication::Before, &record, &root.join(&slug))?;
        // NOREPLACE is atomic even against other creators that don't share our
        // lease. An existing empty directory or symlink must never be overwritten.
        #[cfg(any(target_os = "linux", target_vendor = "apple"))]
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            project.dir(),
            rustix::fs::CWD,
            root.join(&slug),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .context("cannot publish managed view without replacing the destination; setup retained")?;
        #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
        anyhow::bail!("atomic managed project publication is unsupported on this platform");
        File::open(&staging)?.sync_all()?;
        File::open(&root)?.sync_all()?;
        drop(_authority);
        publication(Publication::After, &record, &root.join(&slug))?;
    }
    intent.published = true;
    project::write_json(&record, &intent)?;
    Ok(
        json!({"project":slug,"run":run.id,"path":root.join(&slug),"state":"managed","models_started":false,"operation":intent.operation}),
    )
}

#[cfg(test)]
#[path = "managed_setup_tests.rs"]
mod tests;
