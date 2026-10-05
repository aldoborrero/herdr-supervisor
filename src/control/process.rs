//! The pane wrapper owns a live Child handle, never a recovered PID to signal.
//! SQLite exit receipts survive node restarts; absence of a receipt is uncertainty.
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use rusqlite::{Connection, params};
use serde_json::json;

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
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
    mut command: Command,
) -> Result<()> {
    let started = super::store::now();
    let mut receipt = json!({"attempt":attempt,"epoch":epoch,"wrapper_pid":std::process::id(),
        "started_at":started,"deadline":deadline,"scope":"native_process"});
    c.execute(
        "INSERT INTO runtime_processes VALUES (?1,?2,'spawn_started',?3)",
        params![attempt, epoch, serde_json::to_string(&receipt)?],
    )?;
    command.process_group(0);
    let _terminal = Terminal::new(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            receipt["reason"] = json!("spawn_failed");
            receipt["error"] = json!(error.to_string());
            receipt["finished_at"] = json!(super::store::now());
            c.execute(
                "UPDATE runtime_processes SET state='exited',doc=?2 WHERE attempt=?1",
                params![attempt, serde_json::to_string(&receipt)?],
            )?;
            return Err(error.into());
        }
    };
    let _signals = Signals::install();
    receipt["child_pid"] = json!(child.id());
    // A failed running receipt does not abandon the child. Keep watching it and
    // retry the durable terminal receipt after wait; never infer exit from PID absence.
    if let Err(error) = c.execute(
        "UPDATE runtime_processes SET state='running',doc=?2 WHERE attempt=?1",
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
        let state = c.query_row("SELECT epoch,state FROM jobs WHERE id=?1", [attempt], |r| {
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
        "UPDATE runtime_processes SET state='exited',doc=?3 WHERE attempt=?1 AND epoch=?2",
        params![attempt, epoch, serde_json::to_string(&receipt)?],
    )?;
    ensure!(changed == 1, "exit receipt lost its process identity");
    Ok(())
}
