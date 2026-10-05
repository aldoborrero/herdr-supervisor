//! Projects is a view of one controller run, never a second scheduler.
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::control::{
    api,
    model::{Mutation, digest, identifier},
    runtime,
};
use crate::paths::Ctx;
use crate::project::{self, Project};

const MARKER: &str = "managed.json";

#[derive(Debug)]
pub struct AuthorityLease(File);
impl Drop for AuthorityLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn authority_lease(project: &Project, exclusive: bool) -> Result<AuthorityLease> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(project.state_dir().join("authority.lock"))?;
    ensure!(
        file.metadata()?.is_file(),
        "project authority lock must be regular"
    );
    if exclusive {
        file.try_lock()
            .context("project is in use; finish its current operation before attaching")?;
    } else {
        file.lock_shared()?;
    }
    Ok(AuthorityLease(file))
}

/// An unreadable or malformed marker still excludes all legacy behavior.
pub fn present(project: &Project) -> bool {
    !matches!(std::fs::symlink_metadata(project.state_dir().join(MARKER)),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
}

pub fn require_legacy(project: &Project) -> Result<()> {
    ensure!(
        !present(project),
        "project `{}` is managed by the controller; use `managed status`, `managed apply` or its Projects view",
        project.slug
    );
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub controller: PathBuf,
    pub credential: PathBuf,
    pub run: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_config: Option<PathBuf>,
}

pub fn binding(project: &Project) -> Result<Binding> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(project.state_dir().join(MARKER))
        .context("managed binding unavailable")?;
    ensure!(
        file.metadata()?.is_file(),
        "managed binding must be regular"
    );
    let mut bytes = Vec::new();
    (&mut file).take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "managed binding too large");
    let binding: Binding = serde_json::from_slice(&bytes)
        .context("managed binding is corrupt; legacy fallback refused")?;
    ensure!(
        binding.version == 1
            && binding.controller.is_absolute()
            && binding.credential.is_absolute()
            && binding
                .launch_config
                .as_ref()
                .is_none_or(|p| p.is_absolute()),
        "unsupported managed binding"
    );
    identifier(&binding.run)?;
    Ok(binding)
}

impl Binding {
    pub fn page(
        &self,
        collection: crate::control::pagination::Collection,
        cursor: Option<&str>,
    ) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::ProjectPage {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
                collection,
                cursor: cursor.map(str::to_owned),
            },
        )
    }

    pub fn overview(&self) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::CoordinatorOverview {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
            },
        )
    }

    pub fn context(&self) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::CoordinatorContext {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
            },
        )
    }

    pub fn records(
        &self,
        collection: crate::control::pagination::Collection,
        cursor: Option<String>,
        limit: u32,
    ) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::RunPage {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
                collection,
                cursor,
                limit,
            },
        )
    }

    pub fn record(&self, cursor: String) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::Record {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
                cursor,
            },
        )
    }

    pub fn open_context(&self, profile: Option<String>) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::CoordinatorOpenContext {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
                profile,
            },
        )
    }

    pub fn apply_receipt(&self, operation: &str, command: Mutation) -> Result<Value> {
        identifier(operation)?;
        api::call(
            &self.controller,
            &api::ApiRequest::ProjectApplyReceipt {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
                operation_id: operation.into(),
                command: Box::new(command),
            },
        )
    }

    pub fn apply(&self, operation: &str, command: Mutation) -> Result<Value> {
        identifier(operation)?;
        api::call(
            &self.controller,
            &api::ApiRequest::ProjectApply {
                run: self.run.clone(),
                token: runtime::credential(&self.credential)?,
                operation_id: operation.into(),
                command: Box::new(command),
            },
        )
    }
}

#[cfg(test)]
pub fn context(project: &Project) -> Result<Value> {
    binding(project)?.context()
}

pub fn attach(root: &Path, slug: &str, binding: Binding) -> Result<()> {
    if let Some(path) = &binding.launch_config {
        ensure!(path.is_absolute(), "launch config path must be absolute");
        crate::control::coordinator_launch::read_config(path)?;
    }
    let project = Project::load_unlocked(root, slug)?;
    let _lease = authority_lease(&project, true)?;
    let context = binding.overview()?;
    ensure!(
        context["run"]["id"] == binding.run && context["run"]["project"] == slug,
        "controller run must name this project slug"
    );
    if present(&project) {
        let old = self::binding(&project)?;
        ensure!(
            old.controller == binding.controller && old.run == binding.run,
            "project is already attached to another authority/run"
        );
    } else {
        ensure!(
            crate::thread::list_checked(&project)?.is_empty(),
            "attach requires a project without legacy threads; existing work is not migrated"
        );
        ensure!(
            !project.state_dir().join("coordinator.json").exists(),
            "attach requires a project without a recorded legacy coordinator"
        );
        let tasks = std::fs::read_to_string(project.dir().join("TASKS.md"))?;
        ensure!(
            crate::tasks::parse(&tasks).is_empty(),
            "attach requires an empty legacy task list"
        );
    }
    write_binding(&project, &binding)
}

/// Caller owns the authority lease and has validated the controller run.
pub(crate) fn write_binding(project: &Project, binding: &Binding) -> Result<()> {
    // Marker first: a crash cannot leave a project writable by the old scheduler.
    project::write_json(&project.state_dir().join(MARKER), &binding)?;
    project::write_atomic(&project.dir().join("AGENTS.md"), format!(
        "# Managed Herdr project\n\nThis folder is an operator view of controller run `{}`. Native coordinators must use the generation-scoped credential and context command supplied in their mailer bootstrap. Do not use the operator view's credential or `managed apply` for agent decisions. Projects files are views; never use the legacy thread/task/routine scheduler.\n",binding.run).as_bytes())?;
    Ok(())
}

pub fn change_status(project: &Project, status: project::Status) -> Result<Value> {
    let binding = binding(project)?;
    let context = binding.overview()?;
    let expected_revision = context["run"]["revision"]
        .as_u64()
        .context("run revision missing")?;
    let command = match status {
        project::Status::Paused => Mutation::Pause {
            run: binding.run.clone(),
            expected_revision,
        },
        project::Status::Active => Mutation::Resume {
            run: binding.run.clone(),
            expected_revision,
        },
        _ => bail!("archive/delete is not a controller lifecycle action"),
    };
    let operation = format!("ui-{}", digest(&serde_json::to_vec(&command)?));
    binding.apply_receipt(&operation, command)
}

/// Keep model-provided text inert when rendered in the terminal.
pub fn display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn summary(context: &Value) -> String {
    let active = context["active_attempts"].as_u64().unwrap_or(0);
    let tasks = context["collections"]["tasks"]["count"]
        .as_u64()
        .unwrap_or_else(|| context["tasks"].as_array().map_or(0, Vec::len) as u64);
    format!(
        "{} · {tasks} tasks · {active} active attempts",
        context["run"]["state"].as_str().unwrap_or("unknown")
    )
}

pub fn overview(project: &Project) -> Result<String> {
    let binding = binding(project)?;
    let context = binding.overview()?;
    let page = binding.records(
        crate::control::pagination::Collection::Tasks,
        context["collections"]["tasks"]["cursor"]
            .as_str()
            .map(str::to_owned),
        50,
    )?;
    let mut lines = vec![
        format!("{} · {}", project.slug, summary(&context)),
        display(context["run"]["objective"].as_str().unwrap_or_default()),
    ];
    if let Some(recovery) = recovery_summary(&context) {
        lines.push(recovery);
    }
    if let Some(tasks) = page["items"].as_array() {
        for item in tasks {
            if !item["record"].is_null() {
                lines.push(format!("{}  large record · {} bytes · read managed page {} --collection tasks for its record cursor", item["key"].as_str().unwrap_or_default(),item["record"]["bytes"],project.slug));
                continue;
            }
            let t = &item["value"];
            lines.push(format!(
                "{}  {}  {}",
                t["id"].as_str().unwrap_or_default(),
                t["state"].as_str().unwrap_or_default(),
                display(t["contract"]["title"].as_str().unwrap_or_default())
            ));
        }
    }
    if page["next_cursor"].is_string() {
        lines.push(format!(
            "Showing {} of {} tasks; use managed page {} --collection tasks to continue",
            page["items"].as_array().map_or(0, Vec::len),
            context["collections"]["tasks"]["count"],
            project.slug
        ));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn recovery_summary(context: &Value) -> Option<String> {
    let recovery = &context["repository_recovery"];
    match recovery["state"].as_str()? {
        "released" => Some("Repository released; worktrees and refs retained".into()),
        "awaiting_reconciliation" => Some("Repository awaiting release".into()),
        "blocked" => {
            let reasons: Vec<_> = recovery["blockers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| match v.as_str() {
                    Some("worker_exit_unresolved") => "workers have not confirmed exit",
                    Some("coordinator_exit_unresolved") => "coordinator exit is unconfirmed",
                    Some("repository_creation_unresolved") => "repository setup is unresolved",
                    Some("candidate_preparation_unresolved") => {
                        "candidate preparation is unresolved"
                    }
                    Some("git_apply_unresolved") => "Git update is unresolved",
                    Some("check_exit_unresolved") => "check exit is unconfirmed",
                    Some("checkout_creation_unresolved") => {
                        "verification checkout setup is unresolved"
                    }
                    _ => "an earlier operation is unresolved",
                })
                .collect();
            Some(format!(
                "Repository still reserved: {}",
                if reasons.is_empty() {
                    "waiting for cancellation to finish".into()
                } else {
                    reasons.join(", ")
                }
            ))
        }
        _ => None,
    }
}

/// Launch requests go through the controller; opening an uncertain launch never
/// creates another workspace or invokes the old Projects scheduler.
pub fn open_coordinator(
    ctx: &Ctx,
    project: &Project,
    options: &crate::coordinator::OpenOptions,
) -> Result<()> {
    ensure!(
        !options.rebind,
        "managed coordinator identity is bound by the controller"
    );
    let binding = binding(project)?;
    let context = binding.open_context(options.profile.clone())?;
    if !context["native_coordinator"].is_null() {
        ensure!(
            !options.new && options.profile.is_none(),
            "coordinator exists; retire its generation and wait for the owned process exit before replacement"
        );
        return focus_coordinator(ctx, project, &options.session);
    }
    if let Some(launch) = context["pending_launch"].as_object() {
        ensure!(
            !options.new && options.profile.is_none(),
            "coordinator launch is pending or unresolved; replacement refused"
        );
        println!(
            "coordinator {} · {}{}",
            launch["id"].as_str().unwrap_or_default(),
            launch["state"].as_str().unwrap_or("unknown"),
            launch["last_error"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" · {}", display(s)))
                .unwrap_or_default()
        );
        if launch["last_error_omitted"] == true {
            println!("Launch error exceeds the preview budget; inspect launch diagnostics");
        }
        return Ok(());
    }
    let config = crate::control::coordinator_launch::read_config(binding.launch_config.as_ref().context("automatic launch needs managed attach --launch-config and run-controller --coordinator-config")?)?;
    let profile = context["selected_profile"].as_str().with_context(|| if options.profile.is_some() {
        "coordinator profile is not authorized by this run"
    } else {
        "choose a run-authorized coordinator profile with --profile; managed page --collection profiles lists them"
    })?.to_string();
    let command = Mutation::StartCoordinator {
        run: binding.run.clone(),
        expected_revision: context["run"]["revision"]
            .as_u64()
            .context("run revision missing")?,
        profile,
        config,
    };
    let operation = format!("ui-{}", digest(&serde_json::to_vec(&command)?));
    let result = binding.apply(&operation, command)?;
    println!(
        "coordinator {} reserved; run-controller advances launch and native bootstrap",
        result["id"].as_str().context("launch ID missing")?
    );
    Ok(())
}

pub fn focus_coordinator(
    ctx: &Ctx,
    project: &Project,
    session: &crate::paths::SessionFlags,
) -> Result<()> {
    let context = binding(project)?.overview()?;
    let endpoint = &context["native_coordinator"]["endpoint"];
    let uuid = endpoint["native_session"]
        .as_str()
        .context("no native coordinator is attached")?;
    let runtime = endpoint["runtime"]
        .as_str()
        .context("coordinator runtime missing")?;
    let session = crate::paths::resolve_session(session, ctx.env, ctx.runner)?;
    let herdr = crate::herdr::Herdr::new(ctx.env.herdr_bin(), &session.socket, ctx.runner);
    let agents = herdr.agent_list().map_err(|e| anyhow::anyhow!("{e}"))?;
    let matches = agents
        .iter()
        .filter(|a| a.session_id() == uuid && a.agent == runtime)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "exact coordinator session is not uniquely present in this Herdr server"
    );
    let agent = matches[0];
    let panes = herdr.pane_list().map_err(|e| anyhow::anyhow!("{e}"))?;
    ensure!(
        !agent.terminal_id.is_empty()
            && panes.iter().any(|p| p.pane_id == agent.pane_id
                && p.terminal_id == agent.terminal_id
                && p.workspace_id == agent.workspace_id),
        "coordinator terminal identity changed"
    );
    herdr
        .agent_focus(&agent.pane_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

pub fn project_for_workspace(ctx: &Ctx, workspace: &str, socket: &str) -> Option<String> {
    let projects = project::list_slugs(&ctx.root)
        .into_iter()
        .filter_map(|s| Project::load_view(&ctx.root, &s).ok())
        .filter(present)
        .collect::<Vec<_>>();
    if projects.is_empty() {
        return None;
    }
    let herdr = crate::herdr::Herdr::new(ctx.env.herdr_bin(), socket, ctx.runner);
    let panes = herdr.pane_list().ok()?;
    let agents = herdr.agent_list().ok()?;
    let observations: Vec<_> = agents
        .iter()
        .filter(|a| a.workspace_id == workspace)
        .filter_map(|a| {
            let observation = crate::control::pagination::SessionObservation {
                session: a.session_id().into(),
                runtime: a.agent.clone(),
                workspace: a.workspace_id.clone(),
                pane: a.pane_id.clone(),
                terminal: a.terminal_id.clone(),
            };
            observation.validate().ok().map(|()| observation)
        })
        .collect();
    let mut matches = Vec::new();
    for project in projects {
        let home = project.canonical_dir();
        let in_home = panes.iter().any(|p| {
            p.workspace_id == workspace
                && [&p.cwd, &p.foreground_cwd]
                    .into_iter()
                    .any(|dir| !dir.is_empty() && Path::new(dir) == home)
        });
        let native = || -> Option<bool> {
            let binding = binding(&project).ok()?;
            let token = runtime::credential(&binding.credential).ok()?;
            for chunk in observations.chunks(50) {
                let result = api::call(
                    &binding.controller,
                    &api::ApiRequest::MatchSessions {
                        token: token.clone(),
                        run: binding.run.clone(),
                        observations: chunk.to_vec(),
                    },
                )
                .ok()?;
                if result["matched"] == true {
                    return Some(true);
                }
            }
            Some(false)
        };
        if in_home || native() == Some(true) {
            matches.push(project.slug.clone());
        }
    }
    (matches.len() == 1).then(|| matches.remove(0))
}

#[derive(Args)]
pub struct ManagedArgs {
    #[command(subcommand)]
    action: ManagedAction,
}
#[derive(Subcommand)]
enum ManagedAction {
    /// Save owner-approved local/remote Herdr routes for worker navigation
    FocusConfig {
        slug: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// Focus the exact active native session using this project's node routes
    Focus {
        slug: String,
        attempt: String,
        #[arg(long)]
        expected_epoch: u64,
    },
    /// Read one fragment from a large-record reference returned by a page
    Record {
        slug: String,
        #[arg(long)]
        cursor: String,
    },
    /// Create a run and publish its already-managed project view; retries preserve identity
    Create {
        #[arg(
            long,
            required_unless_present = "spec_json",
            conflicts_with = "spec_json"
        )]
        spec: Option<PathBuf>,
        #[arg(long)]
        spec_json: Option<String>,
    },
    /// Resume the exact saved setup after an interrupted UI or uncertain response
    ResumeSetup { slug: String },
    /// Inspect local setup receipts; does not claim current controller state
    SetupStatus { slug: String },
    /// Attach a fresh project to an existing controller run; does not launch models
    Attach {
        slug: String,
        #[arg(long)]
        controller: PathBuf,
        #[arg(long)]
        credential: PathBuf,
        #[arg(long)]
        run: String,
        /// Owner-approved controller-host launcher configuration
        #[arg(long)]
        launch_config: Option<PathBuf>,
    },
    /// Show bounded controller context; --full requests the legacy aggregate
    Status {
        slug: String,
        #[arg(long)]
        full: bool,
    },
    /// Read complete records from one authorized collection
    Page {
        slug: String,
        #[arg(long, value_enum)]
        collection: crate::control::pagination::Collection,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Apply a decision scoped to this project; request is a JSON file or - for stdin
    Apply {
        slug: String,
        #[arg(long)]
        operation: String,
        #[arg(long, required_unless_present = "request_json")]
        request: Option<PathBuf>,
        #[arg(long, conflicts_with = "request")]
        request_json: Option<String>,
        /// Return a bounded committed-operation receipt; normal output stays compatible
        #[arg(long)]
        receipt: bool,
    },
}

pub fn run(ctx: &Ctx, args: ManagedArgs) -> Result<()> {
    match args.action {
        ManagedAction::FocusConfig { slug, file } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            crate::managed_focus::configure(&project, &file)?;
            println!("focus routes saved for {slug}");
        }
        ManagedAction::Focus {
            slug,
            attempt,
            expected_epoch,
        } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            crate::managed_focus::focus(ctx, &project, &attempt, expected_epoch)?;
        }
        ManagedAction::ResumeSetup { slug } => {
            println!("{}", crate::managed_setup::resume(&ctx.root, &slug)?)
        }
        ManagedAction::SetupStatus { slug } => {
            println!("{}", crate::managed_setup::status(&ctx.root, &slug)?)
        }
        ManagedAction::Create { spec, spec_json } => {
            let spec = if let Some(json) = spec_json {
                crate::managed_setup::parse(&json)?
            } else {
                crate::managed_setup::read_spec(&spec.context("setup specification missing")?)?
            };
            println!("{}", crate::managed_setup::create(&ctx.root, spec)?);
        }
        ManagedAction::Attach {
            slug,
            controller,
            credential,
            run,
            launch_config,
        } => {
            let binding = Binding {
                version: 1,
                controller: std::fs::canonicalize(controller)?,
                credential: std::fs::canonicalize(credential)?,
                launch_config: launch_config.map(std::fs::canonicalize).transpose()?,
                run,
            };
            attach(&ctx.root, &slug, binding)?;
            println!("attached `{slug}` to controller authority");
        }
        ManagedAction::Record { slug, cursor } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            println!("{}", binding(&project)?.record(cursor)?);
        }
        ManagedAction::Status { slug, full } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            let binding = binding(&project)?;
            let context = if full {
                binding.context()?
            } else {
                binding.overview()?
            };
            println!("{}", serde_json::to_string_pretty(&context)?);
        }
        ManagedAction::Page {
            slug,
            collection,
            cursor,
            limit,
        } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            println!("{}", binding(&project)?.records(collection, cursor, limit)?);
        }
        ManagedAction::Apply {
            slug,
            operation,
            request,
            request_json,
            receipt,
        } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            let mut bytes = Vec::new();
            if let Some(json) = request_json {
                bytes = json.into_bytes();
            } else if let Some(request) = request {
                if request == Path::new("-") {
                    std::io::stdin().take(300001).read_to_end(&mut bytes)?;
                } else {
                    File::open(request)?.take(300001).read_to_end(&mut bytes)?;
                }
            }
            ensure!(bytes.len() <= 300000, "managed request too large");
            let command: Mutation = serde_json::from_slice(&bytes)?;
            let binding = binding(&project)?;
            let response = if receipt {
                binding.apply_receipt(&operation, command)?
            } else {
                binding.apply(&operation, command)?
            };
            println!("{response}");
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct Action {
    pub key: char,
    pub label: &'static str,
    pub command: Mutation,
    pub confirm: bool,
}

impl Action {
    pub fn args(&self, slug: &str) -> Result<Vec<String>> {
        let json = serde_json::to_string(&self.command)?;
        Ok(vec![
            "managed".into(),
            "apply".into(),
            slug.into(),
            "--operation".into(),
            format!("ui-{}", digest(json.as_bytes())),
            "--request-json".into(),
            json,
            "--receipt".into(),
        ])
    }
}

fn action(key: char, label: &'static str, command: Value, confirm: bool) -> Result<Action> {
    Ok(Action {
        key,
        label,
        command: serde_json::from_value(command)?,
        confirm,
    })
}

#[cfg(test)]
pub fn panel(project: &Project, section: crate::popup::Section) -> Vec<crate::popup::Row> {
    let view = match context(project) {
        Ok(value) => crate::managed_refresh::View::Current(value),
        Err(error) => crate::managed_refresh::View::Failed(error.to_string()),
    };
    panel_view(&project.slug, section, view)
}

pub fn page_collections(
    section: crate::popup::Section,
) -> &'static [crate::control::pagination::Collection] {
    use crate::{control::pagination::Collection::*, popup::Section};
    match section {
        Section::Threads => &[Attempts],
        Section::Inbox => &[PendingNotices, Questions, Notices],
        Section::Memory => &[Memory, PendingMemory, MemoryProposals],
        Section::Tasks => &[Tasks, Dependencies, Integrations, Checks, Reviews, Results],
        Section::Settings => &[Profiles, Nodes, CoordinatorLaunches],
        Section::Routines => &[PolicyChecks],
    }
}

pub fn page_view(
    slug: &str,
    section: crate::popup::Section,
    collection: crate::control::pagination::Collection,
    view: crate::managed_refresh::View,
) -> Vec<crate::popup::Row> {
    use crate::popup::{Row, RowKind};
    let read_only = !matches!(&view, crate::managed_refresh::View::Current(_));
    let mut rows = panel_view(slug, section, view);
    if !rows
        .iter()
        .any(|row| matches!(row.kind, RowKind::Managed { .. }))
    {
        rows.push(Row {
            header: false,
            text: "Home refresh · v change list".into(),
            color: None,
            kind: RowKind::Managed {
                slug: slug.into(),
                title: "Page navigation".into(),
                doc: json!({"_view_read_only":true}),
                actions: vec![],
            },
        });
    }
    for row in &mut rows {
        if let RowKind::Managed { doc, .. } = &mut row.kind {
            doc["_page_collection"] = json!(collection);
            if read_only {
                doc["_view_read_only"] = Value::Bool(true);
            }
        }
    }
    rows
}

pub fn panel_view(
    slug: &str,
    section: crate::popup::Section,
    view: crate::managed_refresh::View,
) -> Vec<crate::popup::Row> {
    use crate::managed_refresh::View;
    let result = match view {
        View::Current(value) => panel_rows(slug, section, &value),
        View::Stale(value, error) => panel_rows(slug, section, &value).map(|mut rows| {
            // Retain useful context, but never advertise decisions from a view
            // whose last refresh failed. Captured confirmations still carry
            // their exact revisions and are checked by the controller.
            for row in &mut rows {
                if let crate::popup::RowKind::Managed { actions, doc, .. } = &mut row.kind {
                    actions.clear();
                    doc["_view_read_only"] = Value::Bool(true);
                }
            }
            rows.insert(
                0,
                crate::popup::Row {
                    header: true,
                    text: display(&format!("{slug} · last known state · read only: {error}")),
                    color: Some(crossterm::style::Color::Yellow),
                    kind: crate::popup::RowKind::None,
                },
            );
            // Action hints were composed from the original rows. Rebuild each
            // line from the inert title after removing its available actions.
            for row in &mut rows {
                if let crate::popup::RowKind::Managed { title, .. } = &row.kind {
                    row.text = title.clone();
                }
            }
            rows
        }),
        View::Loading => {
            return vec![crate::popup::Row {
                header: true,
                text: format!("{slug} · loading controller state…"),
                color: Some(crossterm::style::Color::DarkGrey),
                kind: crate::popup::RowKind::None,
            }];
        }
        View::Failed(error) => Err(anyhow::anyhow!(error)),
    };
    match result {
        Ok(rows) => rows,
        Err(error) => vec![crate::popup::Row {
            header: false,
            text: format!(
                "{} · controller unavailable: {}",
                slug,
                display(&error.to_string())
            ),
            color: Some(crossterm::style::Color::Red),
            kind: crate::popup::RowKind::Managed {
                slug: slug.into(),
                title: "Controller unavailable".into(),
                doc: json!({"error":error.to_string(),"legacy_fallback":false}),
                actions: vec![],
            },
        }],
    }
}

fn panel_rows(
    slug: &str,
    section: crate::popup::Section,
    c: &Value,
) -> Result<Vec<crate::popup::Row>> {
    use crate::popup::{Row, RowKind, Section};
    let mut rows = vec![Row {
        header: true,
        text: format!("{slug} · {}", summary(c)),
        color: None,
        kind: RowKind::None,
    }];
    let run = c["run"]["id"].as_str().context("run ID missing")?;
    let mut add = |title: String, doc: Value, actions: Vec<Action>| {
        let keys = actions
            .iter()
            .map(|a| format!("{} {}", a.key, a.label))
            .collect::<Vec<_>>()
            .join(" · ");
        rows.push(Row {
            header: false,
            text: display(&format!(
                "{title}{}",
                if keys.is_empty() {
                    String::new()
                } else {
                    format!(" · {keys}")
                }
            )),
            color: None,
            kind: RowKind::Managed {
                slug: slug.into(),
                title: display(&title),
                doc,
                actions,
            },
        });
    };
    match section {
        Section::Threads => {
            for a in c["attempts"].as_array().into_iter().flatten() {
                let mut doc = a.clone();
                doc["_focus_attempt"] = json!(
                    a["state"] != "stopped"
                        && a["resource"].is_object()
                        && a["native_session"].is_string()
                );
                let delivery = &a["cancellation_delivery"];
                let actions = if a["state"] == "cancelling" {
                    if delivery["command"].is_string()
                        && delivery["expires_at"]
                            .as_i64()
                            .is_some_and(|t| t <= jiff::Timestamp::now().as_second())
                        && delivery["state"]
                            .as_str()
                            .is_some_and(|s| ["pending", "expired", "broker_stored"].contains(&s))
                    {
                        vec![action(
                            's',
                            "retry stop",
                            json!({"method":"renew_cancellation","params":{"attempt":a["id"],"expected_epoch":a["epoch"],"expired_command":delivery["command"]}}),
                            true,
                        )?]
                    } else {
                        vec![]
                    }
                } else if a["state"] != "stopped" {
                    vec![action(
                        's',
                        "stop",
                        json!({"method":"stop_attempt","params":{"attempt":a["id"],"expected_epoch":a["epoch"]}}),
                        true,
                    )?]
                } else {
                    vec![]
                };
                add(
                    format!(
                        "{} · {} · {} · {}",
                        a["id"].as_str().unwrap_or_default(),
                        a["role"].as_str().unwrap_or_default(),
                        a["state"].as_str().unwrap_or_default(),
                        a["node"].as_str().unwrap_or_default()
                    ),
                    doc,
                    actions,
                );
            }
        }
        Section::Tasks => {
            if c["run"]["state"] == "active"
                && (c["_page"].is_null() || c["_page"]["collection"] == "tasks")
            {
                add(
                    "Create a task…".into(),
                    json!({"_task_form":{"run":c["run"],"tasks":c["tasks"],"dependencies":c["dependencies"],"task_index_complete":c["task_index_complete"],"checks_allowed":c["checks_allowed"]}}),
                    vec![],
                );
            }
            for t in c["tasks"].as_array().into_iter().flatten() {
                let actions = if ["interrupted", "changes_requested"]
                    .iter()
                    .any(|s| t["state"] == *s)
                {
                    vec![action(
                        'r',
                        "retry",
                        json!({"method":"retry_task","params":{"task":t["id"],"expected_revision":t["revision"],"contract":null}}),
                        true,
                    )?]
                } else {
                    vec![]
                };
                let mut doc = t.clone();
                if t["state"] == "planned" && c["run"]["state"] == "active" {
                    doc["_editable_task"] = Value::Bool(true);
                }
                add(
                    format!(
                        "{} · {}",
                        t["state"].as_str().unwrap_or_default(),
                        t["contract"]["title"].as_str().unwrap_or_default()
                    ),
                    doc,
                    actions,
                );
            }
            for i in c["integrations"].as_array().into_iter().flatten() {
                add(
                    format!(
                        "integration {} · {}",
                        i["id"].as_str().unwrap_or_default(),
                        i["state"].as_str().unwrap_or_default()
                    ),
                    json!({"integration":i,"checks":c["checks"],"reviews":c["reviews"]}),
                    vec![],
                );
            }
            for field in ["dependencies", "checks", "reviews", "results"] {
                if c["_page"]["collection"] == field {
                    for record in c[field].as_array().into_iter().flatten() {
                        let title = if field == "dependencies" {
                            format!(
                                "{} requires {}",
                                record["task"].as_str().unwrap_or_default(),
                                record["predecessor"].as_str().unwrap_or_default()
                            )
                        } else {
                            format!(
                                "{} {} · {}",
                                field,
                                record["id"].as_str().unwrap_or_default(),
                                record["state"].as_str().unwrap_or("record")
                            )
                        };
                        add(title, record.clone(), vec![]);
                    }
                }
            }
        }
        Section::Inbox => {
            for q in c["questions"].as_array().into_iter().flatten() {
                add(
                    format!(
                        "Question · {} · {}",
                        if q["answer"].is_null() {
                            "open"
                        } else {
                            "answered"
                        },
                        q["text"].as_str().unwrap_or_default()
                    ),
                    q.clone(),
                    vec![],
                );
            }
            for n in c["notices"].as_array().into_iter().flatten() {
                let actions = if n["acknowledged_by"].is_null() {
                    vec![action(
                        'a',
                        "acknowledge",
                        json!({"method":"acknowledge_notices","params":{"run":run,"ids":[n["id"]]}}),
                        false,
                    )?]
                } else {
                    vec![]
                };
                add(
                    format!(
                        "{} · {}",
                        n["kind"].as_str().unwrap_or_default(),
                        n["state"].as_str().unwrap_or_default()
                    ),
                    n.clone(),
                    actions,
                );
            }
        }
        Section::Routines => {
            add(
                "Scheduling is controlled by the run policy".into(),
                c["run"]["policy"].clone(),
                vec![],
            );
            for check in c["policy_checks"].as_array().into_iter().flatten() {
                add(
                    format!("Check {}", check["name"].as_str().unwrap_or_default()),
                    check.clone(),
                    vec![],
                );
            }
        }
        Section::Settings => {
            let params = json!({"run":run,"expected_revision":c["run"]["revision"]});
            let mut actions = Vec::new();
            if c["run"]["state"] == "active" {
                actions.push(action(
                    'p',
                    "pause",
                    json!({"method":"pause","params":params}),
                    false,
                )?);
            }
            if c["run"]["state"] == "paused" {
                actions.push(action(
                    'p',
                    "resume",
                    json!({"method":"resume","params":params}),
                    false,
                )?);
            }
            if c["run"]["state"] == "active" || c["run"]["state"] == "paused" {
                actions.push(action(
                    'x',
                    "cancel",
                    json!({"method":"cancel","params":params}),
                    true,
                )?);
            }
            add("Run objective and policy".into(), c["run"].clone(), actions);
            let editable = c["actor_role"] == "owner"
                && (c["run"]["state"] == "active" || c["run"]["state"] == "paused");
            if editable && (c["_page"].is_null() || c["_page"]["collection"] == "profiles") {
                add(
                    "Create a run profile…".into(),
                    json!({"_profile_form":{"run":c["run"],"actor_role":c["actor_role"],"profiles":c["profiles"]}}),
                    vec![],
                );
            }
            if let Some(profiles) = c["profiles"]
                .as_object()
                .or_else(|| c["run"]["policy"]["profiles"].as_object())
            {
                for (name, profile) in profiles {
                    let mut doc = json!({"name":name,"profile":profile});
                    let mut actions = vec![];
                    if editable {
                        doc["_profile_name"] = json!(name);
                        if c["collections"]["profiles"]["count"]
                            .as_u64()
                            .unwrap_or(profiles.len() as u64)
                            > 1
                        {
                            actions.push(action('d',"remove profile",json!({"method":"set_profile","params":{"run":run,"expected_revision":c["run"]["revision"],"name":name,"profile":null}}),true)?);
                        }
                    }
                    add(
                        format!(
                            "Profile {} · {} · limit {}",
                            name,
                            profile["runtime"].as_str().unwrap_or_default(),
                            profile["limit"]
                        ),
                        doc,
                        actions,
                    );
                }
            }
            if let Some(recovery) = recovery_summary(c) {
                add(recovery, c["repository_recovery"].clone(), vec![]);
            }
            if c["_page"].is_null() {
                add("Execution nodes".into(), c["nodes"].clone(), vec![]);
            } else {
                for node in c["nodes"].as_array().into_iter().flatten() {
                    add(
                        format!(
                            "Node {} · {} active",
                            node["id"].as_str().unwrap_or_default(),
                            node["active_attempts"]
                        ),
                        node.clone(),
                        vec![],
                    );
                }
            }
            for launch in c["coordinator_launches"].as_array().into_iter().flatten() {
                add(
                    format!(
                        "Coordinator {} · {}",
                        launch["runtime"].as_str().unwrap_or_default(),
                        launch["state"].as_str().unwrap_or_default()
                    ),
                    launch.clone(),
                    vec![],
                );
            }
            add(
                "Native coordinator".into(),
                c["native_coordinator"].clone(),
                vec![],
            );
        }
        Section::Memory => {
            for (topic, m) in c["memory"]["topics"].as_object().into_iter().flatten() {
                add(
                    format!("{topic} · revision {}", m["revision"]),
                    m.clone(),
                    vec![action(
                        'w',
                        "withdraw",
                        json!({"method":"withdraw_memory","params":{"run":run,"topic":topic,"expected_revision":m["revision"]}}),
                        true,
                    )?],
                );
            }
            for m in c["withdrawn_memory"].as_array().into_iter().flatten() {
                add(
                    format!(
                        "{} · withdrawn at revision {}",
                        m["topic"].as_str().unwrap_or_default(),
                        m["revision"]
                    ),
                    m.clone(),
                    vec![],
                );
            }
            for p in c["memory_proposals"].as_array().into_iter().flatten() {
                let revision = c["memory_topics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|m| m["topic"] == p["topic"])
                    .and_then(|m| m["revision"].as_u64())
                    .unwrap_or(0);
                add(
                    format!(
                        "{} · {}",
                        p["topic"].as_str().unwrap_or_default(),
                        p["status"].as_str().unwrap_or("proposed")
                    ),
                    p.clone(),
                    if p["status"].as_str().unwrap_or("proposed") == "proposed" {
                        vec![action(
                            'a',
                            "approve",
                            json!({"method":"approve_memory","params":{"proposal":p["id"],"expected_revision":revision}}),
                            true,
                        )?]
                    } else {
                        vec![]
                    },
                );
            }
        }
    }
    for reference in c["record_references"].as_array().into_iter().flatten() {
        let title = format!(
            "Large record {} · {} bytes · Enter for retrieval details",
            reference["key"].as_str().unwrap_or_default(),
            reference["record"]["bytes"]
        );
        add(
            title,
            json!({"record":reference["record"],"key":reference["key"],"instructions":"Read managed record SLUG --cursor CURSOR, concatenate text fields in byte-offset order until next_cursor is null, and verify bytes and sha256 before treating it as a complete record.","slug":slug}),
            vec![],
        );
    }
    if rows.len() == 1 {
        rows.push(Row {
            header: true,
            text: "No records in this section".into(),
            color: None,
            kind: RowKind::None,
        });
    }
    if let Some(collection) = c["_page"]["collection"].as_str() {
        let has_records = rows
            .iter()
            .any(|row| matches!(row.kind, RowKind::Managed { .. }));
        let title = format!(
            "{} · page {} · {} shown / {} total · [ previous  ] next  Home first  v list",
            collection.replace('_', " "),
            c["_page"]["number"].as_u64().unwrap_or(1),
            c["_page"]["shown"],
            c["collections"][collection]["count"]
        );
        rows.insert(
            1,
            Row {
                header: has_records,
                text: title.clone(),
                color: None,
                kind: RowKind::Managed {
                    slug: slug.into(),
                    title,
                    doc: json!({"_page_collection":collection,"page":c["_page"]}),
                    actions: vec![],
                },
            },
        );
        for row in &mut rows {
            if let RowKind::Managed { doc, .. } = &mut row.kind {
                doc["_page_collection"] = json!(collection);
            }
        }
    }
    for row in &mut rows {
        if matches!(&row.kind,RowKind::Managed {doc,..} if doc["_focus_attempt"]==true) {
            row.text.push_str(" · f focus");
        }
    }
    Ok(rows)
}
