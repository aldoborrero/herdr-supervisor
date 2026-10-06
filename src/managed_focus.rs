//! Operator-owned node routes and exact native worker navigation.
use crate::{
    control::{
        api,
        model::{Resource, identifier},
        runtime,
    },
    managed,
    paths::Ctx,
    project::{self, Project},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub socket: PathBuf,
    #[serde(default)]
    pub machine: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub nodes: BTreeMap<String, Route>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    controller: PathBuf,
    run: String,
    config: Config,
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "focus routes must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "focus routes exceed 64 KiB");
    Ok(serde_json::from_slice(&bytes)?)
}
fn validate(config: &Config) -> Result<()> {
    ensure!(config.nodes.len() <= 128, "at most 128 focus routes");
    for (node, route) in &config.nodes {
        identifier(node)?;
        ensure!(route.socket.is_absolute(), "focus socket must be absolute");
        if let Some(machine) = &route.machine {
            identifier(machine)?;
        }
    }
    Ok(())
}
pub fn configure(project: &Project, path: &Path) -> Result<()> {
    let config: Config = read(path)?;
    validate(&config)?;
    // Project::load_view already holds the shared authority lease, fencing an
    // attach transition. Serialize only this independent routing configuration.
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(project.state_dir().join("focus-routes.lock"))?;
    ensure!(
        lease.metadata()?.is_file(),
        "focus configuration lease is not a regular file"
    );
    lease
        .try_lock()
        .map_err(|_| anyhow::anyhow!("another process is configuring focus routes"))?;
    let binding = managed::binding(project)?;
    ensure!(
        binding.overview()?["actor_role"] == "owner",
        "owner capability required to configure focus routes"
    );
    let saved = Saved {
        version: 1,
        controller: binding.controller,
        run: binding.run,
        config,
    };
    ensure!(
        serde_json::to_vec_pretty(&saved)?.len() < 65536,
        "saved focus routes exceed 64 KiB"
    );
    project::write_json(&project.state_dir().join("focus.json"), &saved)
}
fn target(binding: &managed::Binding, attempt: &str, epoch: u64) -> Result<Value> {
    api::call(
        &binding.controller,
        &api::ApiRequest::FocusAttempt {
            token: runtime::credential(&binding.credential)?,
            run: binding.run.clone(),
            attempt: attempt.into(),
            expected_epoch: epoch,
        },
    )
}
pub fn focus(ctx: &Ctx, project: &Project, attempt: &str, epoch: u64) -> Result<()> {
    let binding = managed::binding(project)?;
    let saved: Saved = read(&project.state_dir().join("focus.json"))
        .context("configure this project's node routes with managed focus-config first")?;
    ensure!(
        saved.version == 1 && saved.controller == binding.controller && saved.run == binding.run,
        "focus routes belong to a different controller/run"
    );
    validate(&saved.config)?;
    let current = target(&binding, attempt, epoch)?;
    let node = current["node"].as_str().context("focus node missing")?;
    let route = saved
        .config
        .nodes
        .get(node)
        .context("no configured focus route for this node")?;
    let resource: Resource = serde_json::from_value(current["resource"].clone())?;
    let herdr = crate::herdr::Herdr::new(ctx.env.herdr_bin(), &route.socket, ctx.runner);
    let herdr = herdr.on_machine(route.machine.as_deref().unwrap_or_default());
    let agents = herdr.agent_list()?;
    let sessions: Vec<_> = agents
        .iter()
        .filter(|a| {
            a.agent == resource.runtime
                && !(route.machine.is_some()
                    && a.agent_session.as_ref().is_some_and(|s| s.kind == "path"))
                && a.native_session_id().as_deref().ok() == Some(resource.session.as_str())
        })
        .collect();
    ensure!(
        sessions.len() == 1,
        "exact worker session is not uniquely present on the configured route"
    );
    let agent = sessions[0];
    ensure!(
        agent.workspace_id == resource.workspace
            && agent.pane_id == resource.pane
            && agent.terminal_id == resource.terminal,
        "worker resource identity changed"
    );
    let panes = herdr.pane_list()?;
    let matches: Vec<_> = panes
        .iter()
        .filter(|p| p.pane_id == resource.pane)
        .collect();
    ensure!(matches.len() == 1, "worker pane is not uniquely present");
    let pane = matches[0];
    ensure!(
        pane.workspace_id == resource.workspace
            && pane.terminal_id == resource.terminal
            && (pane.cwd == resource.worktree || pane.foreground_cwd == resource.worktree),
        "worker terminal or worktree changed"
    );
    // A stop/retirement during observation must not use the earlier API result.
    ensure!(
        target(&binding, attempt, epoch)? == current,
        "worker binding changed during observation"
    );
    herdr.agent_focus(&resource.pane)?;
    Ok(())
}
