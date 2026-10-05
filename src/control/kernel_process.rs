//! Passive kernel evidence for the original native process; never signal a PID.
//! A different boot/namespace is unknown, not proof that a remote process exited.
use anyhow::Result;
use serde_json::Value;

#[cfg(target_os = "linux")]
pub(super) use linux::{capture, exited};

#[cfg(not(target_os = "linux"))]
pub(super) fn capture(_: u32) -> Result<Value> {
    anyhow::bail!("kernel process recovery is unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub(super) fn exited(_: &Value) -> Result<Option<Value>> {
    anyhow::bail!("kernel process recovery is unsupported on this platform")
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use anyhow::{Context, ensure};
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use std::{
        fs, io,
        os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::fs::MetadataExt,
        },
    };

    #[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Scope {
        boot_id: String,
        pid_namespace_device: u64,
        pid_namespace_inode: u64,
    }
    impl Scope {
        fn current() -> Result<Self> {
            let status = fs::read_to_string("/proc/self/status")?;
            let ids: Vec<_> = status
                .lines()
                .find_map(|s| s.strip_prefix("NSpid:"))
                .context("PID namespace visibility unavailable")?
                .split_whitespace()
                .collect();
            ensure!(
                ids.len() == 1 && ids[0].parse::<u32>()? == std::process::id(),
                "procfs does not describe the current PID namespace"
            );
            let boot_id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .to_owned();
            ensure!(
                uuid::Uuid::parse_str(&boot_id)?.to_string() == boot_id,
                "invalid kernel boot identity"
            );
            let namespace = fs::metadata("/proc/self/ns/pid")?;
            Ok(Self {
                boot_id,
                pid_namespace_device: namespace.dev(),
                pid_namespace_inode: namespace.ino(),
            })
        }
    }
    #[derive(Clone, Debug, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Identity {
        scheme: String,
        scope: Scope,
        pid: u32,
        start_ticks: u64,
    }

    fn start_ticks(stat: &str, pid: u32) -> Result<u64> {
        let (number, _) = stat
            .split_once(" (")
            .context("invalid process stat prefix")?;
        ensure!(number.parse::<u32>()? == pid, "process stat PID differs");
        // comm can contain spaces and parentheses. Fields after its final ') '
        // start at field 3; the original start time is field 22, in clock ticks.
        let (_, fields) = stat
            .rsplit_once(") ")
            .context("invalid process stat command")?;
        Ok(fields
            .split_whitespace()
            .nth(19)
            .context("process start time missing")?
            .parse()?)
    }
    fn pidfd(pid: u32) -> Result<Option<OwnedFd>> {
        ensure!(pid > 0 && pid <= i32::MAX as u32, "invalid process PID");
        // SAFETY: pidfd_open returns a new close-on-exec fd, or -1/errno.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error.into());
        }
        Ok(Some(unsafe { OwnedFd::from_raw_fd(fd as i32) }))
    }
    fn terminated(fd: &OwnedFd) -> Result<bool> {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: valid array of one pollfd, zero timeout; no wait or signal.
        let n = unsafe { libc::poll(&mut poll, 1, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error().into());
        }
        ensure!(
            poll.revents & (libc::POLLERR | libc::POLLNVAL) == 0,
            "invalid pidfd observation"
        );
        Ok(poll.revents & (libc::POLLIN | libc::POLLHUP) != 0)
    }
    pub(crate) fn capture(pid: u32) -> Result<Value> {
        let scope = Scope::current()?;
        let _fd = pidfd(pid)?.context("owned child identity unavailable")?;
        let start_ticks = start_ticks(&fs::read_to_string(format!("/proc/{pid}/stat"))?, pid)?;
        ensure!(
            Scope::current()? == scope,
            "process namespace changed during capture"
        );
        Ok(serde_json::to_value(Identity {
            scheme: "linux-proc-pidfd-v1".into(),
            scope,
            pid,
            start_ticks,
        })?)
    }
    pub(crate) fn exited(value: &Value) -> Result<Option<Value>> {
        let identity: Identity = serde_json::from_value(value.clone())?;
        ensure!(
            identity.scheme == "linux-proc-pidfd-v1",
            "unknown kernel process identity scheme"
        );
        ensure!(
            Scope::current()? == identity.scope,
            "kernel boot or PID namespace differs; process remains unresolved"
        );
        let reason = if let Some(fd) = pidfd(identity.pid)? {
            if terminated(&fd)? {
                Some("pidfd_terminated")
            } else {
                // A hidden/unreadable proc entry is not proof of absence. Only
                // ESRCH from pidfd_open establishes absence in this namespace.
                let stat = fs::read_to_string(format!("/proc/{}/stat", identity.pid))?;
                if start_ticks(&stat, identity.pid)? != identity.start_ticks {
                    Some("pid_reused")
                } else if terminated(&fd)? {
                    Some("pidfd_terminated")
                } else {
                    None
                }
            }
        } else {
            Some("original_pid_absent")
        };
        ensure!(
            Scope::current()? == identity.scope,
            "process namespace changed during observation"
        );
        Ok(reason.map(|reason|json!({"backend":"linux-proc-pidfd-v1","reason":reason,"observed_at":super::super::store::now(),"exit_status_known":false})))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn kernel_process_parser_uses_birth_field_after_full_command_name() {
            let fields = std::iter::once("S".to_string())
                .chain((4..=24).map(|i| i.to_string()))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                start_ticks(&format!("42 (name ) with spaces) {fields}"), 42).unwrap(),
                22
            );
            assert!(start_ticks("43 (wrong) S", 42).is_err());
            assert!(start_ticks("42 (short) S", 42).is_err());
        }
        #[test]
        fn kernel_process_identity_distinguishes_live_exit_reuse_and_foreign_scope() {
            let mut child = std::process::Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .unwrap();
            // The test retains its exact Child handle for cleanup.
            struct Cleanup<'a>(&'a mut std::process::Child);
            impl Drop for Cleanup<'_> {
                fn drop(&mut self) {
                    let _ = self.0.kill();
                    let _ = self.0.wait();
                }
            }
            let child = Cleanup(&mut child);
            let identity = capture(child.0.id()).unwrap();
            assert!(exited(&identity).unwrap().is_none());
            for field in ["boot_id", "pid_namespace_inode", "pid_namespace_device"] {
                let mut foreign = identity.clone();
                foreign["scope"][field] = if field == "boot_id" {
                    json!(uuid::Uuid::new_v4().to_string())
                } else {
                    json!(0)
                };
                assert!(exited(&foreign).is_err());
            }
            let mut reused = identity.clone();
            reused["start_ticks"] = json!(identity["start_ticks"].as_u64().unwrap() + 1);
            assert_eq!(exited(&reused).unwrap().unwrap()["reason"], "pid_reused");
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "observer never signals a reused PID"
            );
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            assert!(exited(&identity).unwrap().is_some());
            let mut malformed = identity;
            malformed["pid"] = json!(0);
            assert!(exited(&malformed).is_err());
        }
    }
}
