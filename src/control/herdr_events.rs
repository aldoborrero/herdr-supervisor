//! Non-authoritative native change hints. Private IPC and task/model delivery
//! stay separate; a stream loss never establishes an agent's exit or readiness.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};

pub type Targets = BTreeMap<PathBuf, BTreeSet<String>>;
/// Endpoints come only from owner configuration and persisted resource plans.
/// Event payloads never select an endpoint or add authority to a resource.
pub fn node_targets(connection: &rusqlite::Connection, configured: &Path) -> Result<Targets> {
    let mut targets = Targets::from([(configured.to_owned(), BTreeSet::new())]);
    collect_targets(
        connection,
        "SELECT COALESCE(json_extract(checkpoint,'$.checkout.socket'),json_extract(checkpoint,'$.socket')), COALESCE(json_extract(checkpoint,'$.checkout.binding.pane'),json_extract(checkpoint,'$.binding.pane')) FROM jobs WHERE state<>'stopped'",
        &mut targets,
    )?;
    Ok(targets)
}
pub fn controller_targets(connection: &rusqlite::Connection) -> Result<Targets> {
    let mut targets = Targets::new();
    collect_targets(
        connection,
        "SELECT json_extract(doc,'$.config.herdr_socket'),json_extract(doc,'$.pane.pane') FROM coordinator_launches WHERE state<>'exited'",
        &mut targets,
    )?;
    Ok(targets)
}
fn collect_targets(
    connection: &rusqlite::Connection,
    query: &str,
    targets: &mut Targets,
) -> Result<()> {
    let mut q = connection.prepare(query)?;
    let rows = q.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
        ))
    })?;
    for row in rows {
        let (socket, pane) = row?;
        if let Some(socket) = socket {
            let socket = PathBuf::from(socket);
            ensure!(
                socket.is_absolute(),
                "saved Herdr endpoint must be absolute"
            );
            let panes = targets.entry(socket).or_default();
            if let Some(pane) = pane {
                panes.insert(pane);
            }
        }
    }
    Ok(())
}
const FRAME_BYTES: usize = 262144;
const SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(3);
const MAX_SOCKETS: usize = 64;
const MAX_PANES: usize = 256;
const LIFECYCLE: &[&str] = &[
    "workspace.created",
    "workspace.updated",
    "workspace.closed",
    "worktree.created",
    "worktree.opened",
    "worktree.removed",
    "pane.created",
    "pane.updated",
    "pane.closed",
    "pane.exited",
    "pane.moved",
    "pane.agent_detected",
];

struct Watch {
    panes: BTreeSet<String>,
    task: JoinHandle<()>,
    state: Arc<AtomicU8>,
}
pub struct Watchers {
    entries: BTreeMap<PathBuf, Watch>,
    tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<()>,
    last: Instant,
    omitted: usize,
    omitted_panes: usize,
}
impl Default for Watchers {
    fn default() -> Self {
        Self::new()
    }
}
impl Watchers {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(1);
        Self {
            entries: BTreeMap::new(),
            tx,
            rx,
            last: Instant::now(),
            omitted: 0,
            omitted_panes: 0,
        }
    }
    pub fn reconcile(&mut self, targets: Targets) {
        self.omitted = targets.len().saturating_sub(MAX_SOCKETS);
        self.omitted_panes = targets
            .values()
            .take(MAX_SOCKETS)
            .map(|p| p.len().saturating_sub(MAX_PANES))
            .sum();
        let wanted: Targets = targets
            .into_iter()
            .take(MAX_SOCKETS)
            .map(|(socket, panes)| (socket, panes.into_iter().take(MAX_PANES).collect()))
            .collect();
        self.entries.retain(|socket, watch| {
            let keep = wanted.get(socket) == Some(&watch.panes) && !watch.task.is_finished();
            if !keep {
                watch.task.abort();
            }
            keep
        });
        for (socket, panes) in wanted {
            if self.entries.contains_key(&socket) {
                continue;
            }
            let state = Arc::new(AtomicU8::new(0));
            let task = tokio::spawn(reconnect(
                socket.clone(),
                panes.clone(),
                self.tx.clone(),
                state.clone(),
            ));
            self.entries.insert(socket, Watch { panes, task, state });
        }
    }
    /// Coalesce bursts and retain the existing bounded health reconciliation.
    pub async fn wait(&mut self) {
        tokio::select! {_=self.rx.recv()=>(),_=tokio::time::sleep(Duration::from_millis(500))=>()}
        tokio::time::sleep_until(self.last + Duration::from_millis(100)).await;
        while self.rx.try_recv().is_ok() {}
        self.last = Instant::now();
    }
    pub fn health(&self) -> Value {
        json!({"watchers":self.entries.len(),"watching":self.entries.values().filter(|w|matches!(w.state.load(Ordering::Relaxed),2|4)).count(),"lifecycle_only":self.entries.values().filter(|w|w.state.load(Ordering::Relaxed)==4).count(),"polling_only_sockets":self.omitted,"polling_only_panes":self.omitted_panes,"authority":"hints_only"})
    }
    pub async fn shutdown(&mut self) {
        for (_, watch) in std::mem::take(&mut self.entries) {
            watch.task.abort();
            let _ = watch.task.await;
        }
    }
}
impl Drop for Watchers {
    fn drop(&mut self) {
        for watch in self.entries.values() {
            watch.task.abort();
        }
    }
}

struct Frames {
    reader: BufReader<UnixStream>,
    bytes: Vec<u8>,
    deadline: Option<Instant>,
}
impl Frames {
    fn new(stream: UnixStream) -> Self {
        Self {
            reader: BufReader::new(stream),
            bytes: Vec::new(),
            deadline: None,
        }
    }
    /// Cancellation retains consumed bytes AND the original completion deadline.
    /// An idle stream has no deadline, but a partial frame has bounded time/space.
    async fn next(&mut self, limit: usize) -> Result<Value> {
        loop {
            let chunk = match self.deadline {
                Some(end) => timeout_at(end, self.reader.fill_buf()).await??,
                None => self.reader.fill_buf().await?,
            };
            ensure!(!chunk.is_empty(), "Herdr stream closed");
            self.deadline
                .get_or_insert_with(|| Instant::now() + DEADLINE);
            let end = chunk.iter().position(|b| *b == b'\n').map(|p| p + 1);
            let n = end.unwrap_or(chunk.len());
            ensure!(
                n <= limit.saturating_sub(self.bytes.len()),
                "Herdr frame exceeds byte limit"
            );
            self.bytes.extend_from_slice(&chunk[..n]);
            self.reader.consume(n);
            if end.is_some() {
                self.deadline = None;
                let bytes = std::mem::take(&mut self.bytes);
                return Ok(serde_json::from_slice(&bytes)?);
            }
        }
    }
}
async fn request(stream: &mut UnixStream, value: Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    timeout(DEADLINE, stream.write_all(&bytes)).await??;
    Ok(())
}
fn acknowledge(value: &Value, id: &str, kind: &str) -> Result<()> {
    ensure!(
        value["id"] == id && value.get("error").is_none() && value["result"]["type"] == kind,
        "unexpected Herdr response"
    );
    Ok(())
}
fn event(value: &Value) -> Result<bool> {
    ensure!(
        value.get("error").is_none(),
        "Herdr subscription interrupted; reconcile after reconnect"
    );
    let name = value["event"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing Herdr event name"))?;
    ensure!(value["data"].is_object(), "invalid Herdr event envelope");
    // 0.9.1 emits snake_case lifecycle names and dotted parameterized names.
    Ok(LIFECYCLE
        .iter()
        .any(|s| *s == name || s.replace('.', "_") == name)
        || name == "pane.agent_status_changed"
        || name == "pane_agent_status_changed")
}
async fn snapshot(socket: &Path) -> Result<()> {
    snapshot_with_id(socket, "supervisor-snapshot").await
}
pub(super) async fn snapshot_with_id(socket: &Path, id: &str) -> Result<()> {
    let mut stream = timeout(DEADLINE, UnixStream::connect(socket)).await??;
    request(
        &mut stream,
        json!({"id":id,"method":"session.snapshot","params":{}}),
    )
    .await?;
    let value = timeout(DEADLINE, Frames::new(stream).next(SNAPSHOT_BYTES)).await??;
    acknowledge(&value, id, "session_snapshot")?;
    let s = &value["result"]["snapshot"];
    ensure!(
        s["protocol"].as_u64().is_some_and(|n| n >= 22) && s["version"].is_string(),
        "Herdr protocol 22 or later required for event snapshots"
    );
    for name in ["agents", "panes", "workspaces", "tabs", "layouts"] {
        ensure!(s[name].is_array(), "incomplete Herdr snapshot");
    }
    // No native event/snapshot is copied into authoritative task or exit state.
    Ok(())
}
async fn subscribe(socket: &Path, panes: &BTreeSet<String>) -> Result<Frames> {
    let mut stream = timeout(DEADLINE, UnixStream::connect(socket)).await??;
    let mut subscriptions: Vec<_> = LIFECYCLE.iter().map(|kind| json!({"type":kind})).collect();
    subscriptions.extend(
        panes
            .iter()
            .map(|pane| json!({"type":"pane.agent_status_changed","pane_id":pane})),
    );
    request(&mut stream,json!({"id":"supervisor-events","method":"events.subscribe","params":{"subscriptions":subscriptions}})).await?;
    let mut reader = Frames::new(stream);
    let ack = timeout(DEADLINE, reader.next(FRAME_BYTES)).await??;
    acknowledge(&ack, "supervisor-events", "subscription_started")?;
    Ok(reader)
}
async fn session(
    socket: &Path,
    panes: &BTreeSet<String>,
    tx: &mpsc::Sender<()>,
    state: &AtomicU8,
) -> Result<()> {
    // Herdr rejects the whole subscription when a named pane has disappeared.
    // Keep lifecycle hints available; ordinary reconciliation still checks all
    // exact identities. A changed target set retries the parameterized request.
    let (mut reader, lifecycle_only) = match subscribe(socket, panes).await {
        Ok(reader) => (reader, false),
        Err(error) if panes.is_empty() => return Err(error),
        Err(_) => (subscribe(socket, &BTreeSet::new()).await?, true),
    };
    state.store(1, Ordering::Relaxed);
    let bootstrap = snapshot(socket);
    tokio::pin!(bootstrap);
    // Drain concurrently so a burst during snapshot cannot grow an application
    // queue. A fresh authoritative reconciliation follows snapshot completion.
    loop {
        tokio::select! {
            result=&mut bootstrap=>{result?;break;},
            incoming=reader.next(FRAME_BYTES)=>{event(&incoming?)?;},
        }
    }
    state.store(if lifecycle_only { 4 } else { 2 }, Ordering::Relaxed);
    let _ = tx.try_send(());
    loop {
        if event(&reader.next(FRAME_BYTES).await?)? {
            let _ = tx.try_send(());
        }
        tokio::task::yield_now().await;
    }
}
async fn reconnect(
    socket: PathBuf,
    panes: BTreeSet<String>,
    tx: mpsc::Sender<()>,
    state: Arc<AtomicU8>,
) {
    let mut delay = Duration::from_secs(1);
    let mut reported = false;
    loop {
        state.store(0, Ordering::Relaxed);
        let start = Instant::now();
        let result = session(&socket, &panes, &tx, &state).await;
        state.store(3, Ordering::Relaxed);
        let _ = tx.try_send(());
        if !reported {
            if let Err(error) = result {
                eprintln!(
                    "Herdr event stream unavailable at {}: {}; continuing health reconciliation",
                    socket.display(),
                    error
                );
            }
            reported = true;
        }
        if start.elapsed() >= Duration::from_secs(30) {
            delay = Duration::from_secs(1);
            reported = false;
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
#[path = "herdr_events_tests.rs"]
mod tests;
