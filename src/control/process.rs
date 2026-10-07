//! The pane wrapper owns a live Child handle, never a recovered PID to signal.
//! SQLite exit receipts survive node restarts; absence of a receipt is uncertainty.
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::json;

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

/// Only the dedicated wrapper uses this. An inherited SIG_IGN/SA_NOCLDWAIT
/// would auto-reap a child, invalidating both Child ownership and birth capture.
struct ChildReaping(libc::sigaction);
impl ChildReaping {
    fn install() -> Result<Self> {
        // SAFETY: initialized sigaction structures, a valid empty signal mask,
        // and a process-wide disposition in the dedicated native wrapper only.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_sigaction = libc::SIG_DFL;
            if libc::sigaction(libc::SIGCHLD, &action, &mut old) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(Self(old))
        }
    }
}
impl Drop for ChildReaping {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGCHLD, &self.0, std::ptr::null_mut());
        }
    }
}

struct Signals(Vec<(libc::c_int, libc::sighandler_t)>);
impl Signals {
    fn install() -> Self {
        STOP.store(false, Ordering::Relaxed);
        let mut old = Vec::new();
        // Only the dedicated wrapper calls this, after spawning its child.
        for signal in [libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: handler performs only a lock-free atomic store.
            old.push((signal, unsafe {
                libc::signal(signal, stop_signal as *const () as libc::sighandler_t)
            }));
        }
        for signal in [libc::SIGINT, libc::SIGQUIT] {
            // Terminal interrupts belong to the native foreground group.
            old.push((signal, unsafe { libc::signal(signal, libc::SIG_IGN) }));
        }
        Self(old)
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        for &(signal, previous) in &self.0 {
            unsafe {
                libc::signal(signal, previous);
            }
        }
    }
}

struct Terminal(libc::pid_t);
impl Terminal {
    fn new(command: &mut Command) -> Self {
        // A pipe/non-controlling stdin needs no terminal handover.
        let foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if foreground > 0 {
            // SAFETY: only async-signal-safe syscalls, with no allocation or locks.
            unsafe {
                command.pre_exec(|| {
                    let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                    let result = libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpid());
                    let error = std::io::Error::last_os_error();
                    libc::signal(libc::SIGTTOU, old);
                    if result < 0 { Err(error) } else { Ok(()) }
                });
            }
        }
        Self(foreground)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if self.0 > 0 {
            // SAFETY: restore the original foreground group even from background.
            unsafe {
                let old = libc::signal(libc::SIGTTOU, libc::SIG_IGN);
                libc::tcsetpgrp(libc::STDIN_FILENO, self.0);
                libc::signal(libc::SIGTTOU, old);
            }
        }
    }
}

pub fn run(
    c: Connection,
    lease: super::api::Lease,
    attempt: &str,
    epoch: u64,
    deadline: i64,
    command: Command,
) -> Result<()> {
    run_owned(c, lease, attempt, epoch, deadline, command, false)
}

pub fn run_coordinator(
    c: Connection,
    lease: super::api::Lease,
    launch: &str,
    generation: u64,
    deadline: i64,
    command: Command,
) -> Result<()> {
    run_owned(c, lease, launch, generation, deadline, command, true)
}

fn run_owned(
    c: Connection,
    lease: super::api::Lease,
    attempt: &str,
    epoch: u64,
    deadline: i64,
    mut command: Command,
    coordinator: bool,
) -> Result<()> {
    let _reaping = ChildReaping::install()?;
    let (table, key, version) = if coordinator {
        ("coordinator_processes", "launch", "generation")
    } else {
        ("runtime_processes", "attempt", "epoch")
    };
    let owner_nonce = super::model::id();
    let state = std::path::Path::new(c.path().context("process database path missing")?)
        .parent()
        .context("process state directory missing")?;
    // Held only by this wrapper, across spawn/watch/terminal-receipt persistence.
    // The File is close-on-exec, so the native child cannot preserve a dead owner's lease.
    let _owner = super::api::lock(state, &format!("native-owner-{owner_nonce}.lock"))?;
    let started = super::store::now();
    let mut receipt = json!({"attempt":attempt,"epoch":epoch,"wrapper_pid":std::process::id(),
        "started_at":started,"deadline":deadline,"scope":"native_process","owner_nonce":owner_nonce});
    if coordinator {
        receipt.as_object_mut().unwrap().remove("attempt");
        receipt["launch"] = json!(attempt);
        receipt["generation"] = json!(epoch);
    }
    let tx = c.unchecked_transaction()?;
    if coordinator {
        super::coordinator_launch::claim_wrapper(&tx, attempt, epoch)?;
    }
    tx.execute(
        &format!("INSERT INTO {table} VALUES (?1,?2,'spawn_started',?3)"),
        params![attempt, epoch, serde_json::to_string(&receipt)?],
    )?;
    tx.commit()?;
    command.process_group(0);
    let _terminal = Terminal::new(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            receipt["reason"] = json!("spawn_failed");
            receipt["error"] = json!(error.to_string());
            receipt["finished_at"] = json!(super::store::now());
            c.execute(
                &format!("UPDATE {table} SET state='exited',doc=?2 WHERE {key}=?1"),
                params![attempt, serde_json::to_string(&receipt)?],
            )?;
            return Err(error.into());
        }
    };
    let _signals = Signals::install();
    receipt["child_pid"] = json!(child.id());
    // The owned Child is still unreaped here, so this birth identity cannot be
    // substituted by PID reuse. Unsupported kernels leave recovery unavailable.
    receipt["kernel_identity"] =
        super::kernel_process::capture(child.id()).unwrap_or(serde_json::Value::Null);
    // A failed running receipt does not abandon the child. Keep watching it and
    // retry the durable terminal receipt after wait, retaining the exact Child.
    if let Err(error) = c.execute(
        &format!("UPDATE {table} SET state='running',doc=?2 WHERE {key}=?1"),
        params![attempt, serde_json::to_string(&receipt)?],
    ) {
        eprintln!("runtime start receipt: {error}");
    }
    drop(lease);
    let mut stopping: Option<Instant> = None;
    let mut killed = false;
    let status = loop {
        // try_wait only reaps on Some. Once reaped, never signal this numeric PID again.
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let query = if coordinator {
            "SELECT l.generation,CASE WHEN r.state IN ('cancelling','cancelled','completed') OR NOT EXISTS(SELECT 1 FROM principals p WHERE p.run=l.run AND p.role='supervisor' AND p.generation=l.generation AND p.active=1) THEN 'cancelling' ELSE l.state END FROM coordinator_launches l JOIN runs r ON r.id=l.run WHERE l.id=?1"
        } else {
            "SELECT epoch,state FROM jobs WHERE id=?1"
        };
        let state = c.query_row(query, [attempt], |r| {
            Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
        });
        let reason = match state {
            Ok((current, ref state)) if current != epoch || state == "cancelling" => {
                Some("cancelled")
            }
            Err(_) => Some("authority_unavailable"),
            _ if STOP.load(Ordering::Relaxed) => Some("wrapper_signal"),
            _ if super::store::now() >= deadline => Some("deadline"),
            _ => None,
        };
        if stopping.is_none()
            && let Some(reason) = reason
        {
            receipt["reason"] = json!(reason);
            // The unreaped child pins its PID; only its newly-created group is targeted.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM);
            }
            stopping = Some(Instant::now());
        }
        if !killed && stopping.is_some_and(|at| at.elapsed() >= Duration::from_secs(2)) {
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            killed = true;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if receipt.get("reason").is_none() {
        receipt["reason"] = json!("exit");
    }
    receipt["finished_at"] = json!(super::store::now());
    receipt["code"] = json!(status.code());
    receipt["signal"] = json!(status.signal());
    receipt["kill_escalated"] = json!(killed);
    let changed = c.execute(
        &format!("UPDATE {table} SET state='exited',doc=?3 WHERE {key}=?1 AND {version}=?2"),
        params![attempt, epoch, serde_json::to_string(&receipt)?],
    )?;
    ensure!(changed == 1, "exit receipt lost its process identity");
    Ok(())
}

/// Positive evidence of a live wrapper; never infer child exit from an unlocked lease.
/// Missing/legacy evidence is an error, not permission to recover a running receiver.
pub(super) fn require_live_owner(
    state: &std::path::Path,
    receipt: &serde_json::Value,
) -> Result<()> {
    ensure!(
        recovery_owner(state, receipt)?.is_none(),
        "native wrapper is not holding its ownership lease"
    );
    Ok(())
}

pub(super) fn recovery_owner(
    state: &std::path::Path,
    receipt: &serde_json::Value,
) -> Result<Option<super::api::Lease>> {
    let nonce = receipt["owner_nonce"]
        .as_str()
        .context("wrapper ownership evidence unavailable")?;
    ensure!(
        uuid::Uuid::parse_str(nonce)?.to_string() == nonce,
        "invalid owner nonce"
    );
    super::api::existing_lock(state, &format!("native-owner-{nonce}.lock"))
}
