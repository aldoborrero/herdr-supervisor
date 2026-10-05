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
    pub fn context(&self) -> Result<Value> {
        api::call(
            &self.controller,
            &api::ApiRequest::CoordinatorContext {
                token: runtime::credential(&self.credential)?,
                run: self.run.clone(),
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
    let context = binding.context()?;
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
    // Marker first: a crash cannot leave a project writable by the old scheduler.
    project::write_json(&project.state_dir().join(MARKER), &binding)?;
    project::write_atomic(&project.dir().join("AGENTS.md"), format!(
        "# Managed Herdr project\n\nThis folder is an operator view of controller run `{}`. Native coordinators must use the generation-scoped credential and context command supplied in their mailer bootstrap. Do not use the operator view's credential or `managed apply` for agent decisions. Projects files are views; never use the legacy thread/task/routine scheduler.\n",binding.run).as_bytes())?;
    Ok(())
}

pub fn change_status(project: &Project, status: project::Status) -> Result<Value> {
    let binding = binding(project)?;
    let context = binding.context()?;
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
    binding.apply(&operation, command)
}

/// Keep model-provided text inert when rendered in the terminal.
pub fn display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn summary(context: &Value) -> String {
    let active = context["active_attempts"].as_u64().unwrap_or(0);
    let tasks = context["tasks"].as_array().map_or(0, Vec::len);
    format!(
        "{} · {tasks} tasks · {active} active attempts",
        context["run"]["state"].as_str().unwrap_or("unknown")
    )
}

pub fn overview(project: &Project) -> Result<String> {
    let context = context(project)?;
    let mut lines = vec![
        format!("{} · {}", project.slug, summary(&context)),
        display(context["run"]["objective"].as_str().unwrap_or_default()),
    ];
    if let Some(tasks) = context["tasks"].as_array() {
        for t in tasks {
            lines.push(format!(
                "{}  {}  {}",
                t["id"].as_str().unwrap_or_default(),
                t["state"].as_str().unwrap_or_default(),
                display(t["contract"]["title"].as_str().unwrap_or_default())
            ));
        }
    }
    Ok(format!("{}\n", lines.join("\n")))
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
    let context = binding.context()?;
    if !context["native_coordinator"].is_null() {
        ensure!(
            !options.new && options.profile.is_none(),
            "coordinator exists; retire its generation and wait for the owned process exit before replacement"
        );
        return focus_coordinator(ctx, project, &options.session);
    }
    if let Some(launch) = context["coordinator_launches"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|l| l["state"] != "exited")
    {
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
        return Ok(());
    }
    let config = crate::control::coordinator_launch::read_config(binding.launch_config.as_ref().context("automatic launch needs managed attach --launch-config and run-controller --coordinator-config")?)?;
    let profiles = context["run"]["policy"]["profiles"]
        .as_object()
        .context("run profiles missing")?;
    let profile = match &options.profile {
        Some(name) => name.clone(),
        None if profiles.len() == 1 => profiles.keys().next().unwrap().clone(),
        None => bail!("choose a run-authorized coordinator profile with --profile"),
    };
    ensure!(
        profiles.contains_key(&profile),
        "coordinator profile is not authorized by this run"
    );
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
    let context = context(project)?;
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
            let c = context(&project).ok()?;
            Some(
                agents
                    .iter()
                    .filter(|a| a.workspace_id == workspace && !a.session_id().is_empty())
                    .any(|a| {
                        c["native_coordinator"]["endpoint"]["native_session"] == a.session_id()
                            || c["attempts"].as_array().into_iter().flatten().any(|t| {
                                t["state"] != "stopped"
                                    && t["native_session"] == a.session_id()
                                    && t["resource"]["pane"] == a.pane_id
                                    && t["resource"]["terminal"] == a.terminal_id
                            })
                    }),
            )
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
    /// Show current controller context; unavailable authority never falls back to files
    Status { slug: String },
    /// Apply a decision scoped to this project; request is a JSON file or - for stdin
    Apply {
        slug: String,
        #[arg(long)]
        operation: String,
        #[arg(long, required_unless_present = "request_json")]
        request: Option<PathBuf>,
        #[arg(long, conflicts_with = "request")]
        request_json: Option<String>,
    },
}

pub fn run(ctx: &Ctx, args: ManagedArgs) -> Result<()> {
    match args.action {
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
        ManagedAction::Status { slug } => {
            let project = Project::load_view(&ctx.root, &slug)?;
            println!("{}", serde_json::to_string_pretty(&context(&project)?)?);
        }
        ManagedAction::Apply {
            slug,
            operation,
            request,
            request_json,
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
            println!("{}", binding(&project)?.apply(&operation, command)?);
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

pub fn panel(project: &Project, section: crate::popup::Section) -> Vec<crate::popup::Row> {
    match context(project).and_then(|context| panel_rows(&project.slug, section, &context)) {
        Ok(rows) => rows,
        Err(error) => vec![crate::popup::Row {
            header: false,
            text: format!(
                "{} · controller unavailable: {}",
                project.slug,
                display(&error.to_string())
            ),
            color: Some(crossterm::style::Color::Red),
            kind: crate::popup::RowKind::Managed {
                slug: project.slug.clone(),
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
                let actions = if a["state"] != "stopped" {
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
                    a.clone(),
                    actions,
                );
            }
        }
        Section::Tasks => {
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
                add(
                    format!(
                        "{} · {}",
                        t["state"].as_str().unwrap_or_default(),
                        t["contract"]["title"].as_str().unwrap_or_default()
                    ),
                    t.clone(),
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
        }
        Section::Inbox => {
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
        Section::Routines => add(
            "Scheduling is controlled by the run policy".into(),
            c["run"]["policy"].clone(),
            vec![],
        ),
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
            add("Execution nodes".into(), c["nodes"].clone(), vec![]);
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
            for p in c["memory_proposals"].as_array().into_iter().flatten() {
                let revision = c["memory_topics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|m| m["topic"] == p["topic"])
                    .and_then(|m| m["revision"].as_u64())
                    .unwrap_or(0);
                add(
                    format!("{} · proposed", p["topic"].as_str().unwrap_or_default()),
                    p.clone(),
                    vec![action(
                        'a',
                        "approve",
                        json!({"method":"approve_memory","params":{"proposal":p["id"],"expected_revision":revision}}),
                        true,
                    )?],
                );
            }
        }
    }
    if rows.len() == 1 {
        rows.push(Row {
            header: true,
            text: "No records in this section".into(),
            color: None,
            kind: RowKind::None,
        });
    }
    Ok(rows)
}
