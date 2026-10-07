//! Every external command (herdr, git, gh, ssh, scp, rsync, sh) goes through `Runner`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub stdin: Option<String>,
    pub timeout: Duration,
    /// Spawn in its own process group and kill the whole group on timeout.
    pub own_group: bool,
}

impl Cmd {
    pub fn new(program: impl Into<String>, timeout: Duration) -> Self {
        Cmd {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            cwd: None,
            stdin: None,
            timeout,
            own_group: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn env_remove(mut self, key: impl Into<String>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.stdin = Some(text.into());
        self
    }

    pub fn own_group(mut self) -> Self {
        self.own_group = true;
        self
    }

    /// The command as one line; the scripted fake matches on it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn display(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// `None` when the process was killed (timeout or signal).
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// stderr when it has text, else stdout, trimmed; for error messages.
    pub fn error_text(&self) -> String {
        if self.timed_out {
            return "timed out".to_string();
        }
        let text = if self.stderr.trim().is_empty() {
            self.stdout.trim()
        } else {
            self.stderr.trim()
        };
        text.to_string()
    }
}

pub trait Runner {
    /// Only an error containing `SpawnFailure` proves the program did not run.
    /// Other errors can happen after spawning. Non-zero exits and timeouts are
    /// `Ok(Output)` and do not prove absence of external effects.
    fn run(&self, cmd: &Cmd) -> Result<Output>;

    /// One JSON line to a herdr socket, one line back. The single exception to
    /// "talk to herdr through its CLI" (client decision during the build):
    /// herdr 0.9.1 has no CLI command for `agent.view.set` / `agent.view.clear`.
    fn socket_request(&self, socket: &Path, line: &str, timeout: Duration) -> Result<String>;

    /// Runs `cmd` on this process's terminal (an agent `open` starts in its own
    /// pane) and waits for it; `cmd.timeout` and `cmd.stdin` are ignored.
    /// While it runs, `poll` is called about twice a second until it returns
    /// true. Returns the exit code, `None` when a signal ended it.
    fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>>;
}

pub struct RealRunner;

#[derive(Debug)]
pub struct SpawnFailure {
    program: String,
    source: std::io::Error,
}
impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not run `{}`: {}", self.program, self.source)
    }
}
impl std::error::Error for SpawnFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

const POLL: Duration = Duration::from_millis(20);

impl Runner for RealRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        if cmd.own_group {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|source| SpawnFailure {
            program: cmd.program.clone(),
            source,
        })?;

        let result = collect_output(&mut child, cmd, Instant::now() + cmd.timeout);
        if result.is_err() && matches!(child.try_wait(), Ok(None)) {
            // A setup/I/O failure happened after spawn. Reap only our still-owned
            // child; the error deliberately carries no SpawnFailure marker.
            kill(&mut child, cmd.own_group);
            let _ = child.wait();
        }
        result
    }

    fn socket_request(&self, socket: &Path, line: &str, timeout: Duration) -> Result<String> {
        socket_round_trip(socket, line, timeout)
    }

    fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        for key in &cmd.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }
        if let Some(cwd) = &cmd.cwd {
            // This process leads the pane's foreground group, and Herdr reports
            // the leader's directory as the pane's `foreground_cwd`: it moves too.
            std::env::set_current_dir(cwd)
                .with_context(|| format!("could not enter {}", cwd.display()))?;
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("could not run `{}`", cmd.program))?;
        // Ctrl-C and Ctrl-\ reach the whole foreground group: they are the
        // agent's to handle, and this process must outlive it so the shell
        // does not take the terminal back from a running agent.
        let _ignored = IgnoreInterrupts::new();
        let mut polling = true;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if polling {
                polling = !poll();
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        Ok(status.code())
    }
}

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

const SIGINT: i32 = 2;
const SIGQUIT: i32 = 3;
const SIG_IGN: usize = 1;

/// SIGINT and SIGQUIT ignored in this process (set after the child's exec, so
/// the child keeps the default), restored on drop.
struct IgnoreInterrupts(usize, usize);

impl IgnoreInterrupts {
    fn new() -> Self {
        // SAFETY: plain signal(2) calls with the ignore disposition.
        unsafe { IgnoreInterrupts(signal(SIGINT, SIG_IGN), signal(SIGQUIT, SIG_IGN)) }
    }
}

impl Drop for IgnoreInterrupts {
    fn drop(&mut self) {
        // SAFETY: restores the dispositions `new` returned.
        unsafe {
            signal(SIGINT, self.0);
            signal(SIGQUIT, self.1);
        }
    }
}

fn socket_round_trip(socket: &Path, line: &str, timeout: Duration) -> Result<String> {
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("could not connect to {}", socket.display()))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply)
}

/// Pipe completion is part of the same deadline as child execution. There are
/// no detached reader/writer threads waiting on inherited descriptors after it.
fn collect_output(child: &mut std::process::Child, cmd: &Cmd, deadline: Instant) -> Result<Output> {
    let mut input = child.stdin.take();
    let mut output = child.stdout.take();
    let mut error = child.stderr.take();
    if let Some(pipe) = &input {
        nonblocking(pipe)?;
    }
    if let Some(pipe) = &output {
        nonblocking(pipe)?;
    }
    if let Some(pipe) = &error {
        nonblocking(pipe)?;
    }
    let mut remaining = cmd.stdin.as_deref().unwrap_or("").as_bytes();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    let timed_out = loop {
        if status.is_none() {
            status = child.try_wait()?;
        }
        if status.is_some() && input.is_none() && output.is_none() && error.is_none() {
            break false;
        }
        if Instant::now() >= deadline {
            // Never signal a numeric PID/group after reaping its leader: a
            // descendant retaining a pipe does not restore ownership of that PID.
            if status.is_none() {
                kill(child, cmd.own_group);
                let _ = child.wait();
            }
            break true;
        }
        let mut progress = drain(&mut output, &mut stdout);
        progress |= drain(&mut error, &mut stderr);
        if remaining.is_empty() {
            progress |= input.take().is_some();
        } else if let Some(pipe) = input.as_mut() {
            match pipe.write(&remaining[..remaining.len().min(65536)]) {
                Ok(0) => {
                    input = None;
                    progress = true;
                }
                Ok(n) => {
                    remaining = &remaining[n..];
                    progress = true;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => {
                    input = None;
                    progress = true;
                }
            }
        }
        if !progress {
            std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    };
    Ok(Output {
        code: if timed_out {
            None
        } else {
            status.and_then(|s| s.code())
        },
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    // SAFETY: the pipe owns this live descriptor throughout both fcntl calls.
    unsafe {
        let fd = pipe.as_raw_fd();
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

fn drain<R: Read>(pipe: &mut Option<R>, bytes: &mut Vec<u8>) -> bool {
    let Some(reader) = pipe.as_mut() else {
        return false;
    };
    let mut progress = false;
    let mut buffer = [0; 65536];
    // Bound each batch so a continuously chatty stream cannot starve the other
    // stream, stdin, process observation or the deadline check.
    for _ in 0..16 {
        match reader.read(&mut buffer) {
            Ok(0) => {
                *pipe = None;
                return true;
            }
            Ok(n) => {
                bytes.extend_from_slice(&buffer[..n]);
                progress = true;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                break;
            }
            Err(_) => {
                *pipe = None;
                return true;
            }
        }
    }
    progress
}

fn kill(child: &mut std::process::Child, own_group: bool) {
    if own_group {
        // The child is its group's leader, so its pid is the pgid. Grandchildren
        // hold the pipes open; killing only the child would leave readers hanging.
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(200));
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;

    type Matcher = Box<dyn Fn(&Cmd) -> bool>;
    type Responder = Box<dyn Fn(&Cmd) -> Result<Output>>;

    /// A scripted runner: the first rule whose matcher accepts the command
    /// answers it. Every command is recorded, matched or not.
    #[derive(Default)]
    pub struct FakeRunner {
        rules: RefCell<Vec<(Matcher, Responder)>>,
        pub calls: RefCell<Vec<Cmd>>,
        /// (socket, request line) of every socket request.
        pub socket_requests: RefCell<Vec<(PathBuf, String)>>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answer commands whose display line contains `needle`.
        pub fn on(&self, needle: &str, output: Output) -> &Self {
            let needle = needle.to_string();
            self.rules.borrow_mut().push((
                Box::new(move |cmd| cmd.display().contains(&needle)),
                Box::new(move |_| Ok(output.clone())),
            ));
            self
        }

        pub fn on_fn(
            &self,
            matcher: impl Fn(&Cmd) -> bool + 'static,
            answer: impl Fn(&Cmd) -> Result<Output> + 'static,
        ) -> &Self {
            self.rules
                .borrow_mut()
                .push((Box::new(matcher), Box::new(answer)));
            self
        }

        pub fn count(&self, needle: &str) -> usize {
            self.calls
                .borrow()
                .iter()
                .filter(|cmd| cmd.display().contains(needle))
                .count()
        }
    }

    pub fn ok(stdout: &str) -> Output {
        Output {
            code: Some(0),
            stdout: stdout.to_string(),
            ..Output::default()
        }
    }

    pub fn fail(code: i32, stderr: &str) -> Output {
        Output {
            code: Some(code),
            stderr: stderr.to_string(),
            ..Output::default()
        }
    }

    pub fn timeout() -> Output {
        Output {
            timed_out: true,
            ..Output::default()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, cmd: &Cmd) -> Result<Output> {
            self.calls.borrow_mut().push(cmd.clone());
            for (matcher, answer) in self.rules.borrow().iter() {
                if matcher(cmd) {
                    return answer(cmd);
                }
            }
            anyhow::bail!("FakeRunner: no rule for `{}`", cmd.display())
        }

        fn socket_request(&self, socket: &Path, line: &str, _timeout: Duration) -> Result<String> {
            self.socket_requests
                .borrow_mut()
                .push((socket.to_path_buf(), line.to_string()));
            Ok(r#"{"id":"hp","result":{"type":"agent_view","active":true}}"#.to_string())
        }

        /// The first matching rule answers (it may change what later calls
        /// see, as a starting agent does), then `poll` runs once and the
        /// command exits with the rule's code.
        fn run_foreground(&self, cmd: &Cmd, poll: &mut dyn FnMut() -> bool) -> Result<Option<i32>> {
            self.calls.borrow_mut().push(cmd.clone());
            let out = {
                let rules = self.rules.borrow();
                let Some((_, answer)) = rules.iter().find(|(matcher, _)| matcher(cmd)) else {
                    anyhow::bail!("FakeRunner: no rule for `{}`", cmd.display())
                };
                answer(cmd)?
            };
            poll();
            Ok(out.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_exit_code() {
        let out = RealRunner
            .run(
                &Cmd::new("sh", Duration::from_secs(5))
                    .args(["-c", "echo hi; echo err >&2; exit 3"]),
            )
            .unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        assert!(!out.success());
    }

    #[test]
    fn passes_stdin() {
        let out = RealRunner
            .run(&Cmd::new("cat", Duration::from_secs(5)).stdin("hello"))
            .unwrap();
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn deadline_covers_inherited_pipes_after_parent_timeout_or_exit() {
        for script in ["sleep 2 & wait", "sleep 2 & exit 0"] {
            let at = Instant::now();
            let out = RealRunner
                .run(&Cmd::new("sh", Duration::from_millis(50)).args(["-c", script]))
                .unwrap();
            assert!(
                at.elapsed() < Duration::from_secs(1),
                "inherited pipe exceeded deadline"
            );
            assert!(out.timed_out);
            assert!(!out.success());
            assert_eq!(out.code, None);
        }
    }

    #[test]
    fn deadline_covers_blocked_stdin_inherited_by_descendant() {
        let at = Instant::now();
        let out = RealRunner
            .run(
                &Cmd::new("sh", Duration::from_millis(50))
                    .args(["-c", "exec 3<&0; sleep 2 <&3 >/dev/null 2>&1 & exit 0"])
                    .stdin("x".repeat(2 * 1024 * 1024)),
            )
            .unwrap();
        assert!(
            at.elapsed() < Duration::from_secs(1),
            "stdin writer exceeded deadline"
        );
        assert!(out.timed_out);
        assert!(!out.success());
    }

    #[test]
    fn nonblocking_pipes_preserve_large_utf8_duplex_output() {
        let body = "mensaje 🦀\n".repeat(200_000);
        let out = RealRunner
            .run(
                &Cmd::new("sh", Duration::from_secs(10))
                    .args(["-c", "printf 'diagnostic\\n' >&2; cat"])
                    .stdin(&body),
            )
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, body);
        assert_eq!(out.stderr, "diagnostic\n");
    }

    #[test]
    fn missing_program_is_an_error() {
        assert!(
            RealRunner
                .run(&Cmd::new("hp-no-such-program", Duration::from_secs(1)))
                .unwrap_err()
                .downcast_ref::<SpawnFailure>()
                .is_some()
        );
    }

    #[test]
    fn times_out_a_chatty_child() {
        // `yes` fills the pipe far past its buffer; the reader threads keep it
        // drained so the deadline still fires.
        let start = Instant::now();
        let out = RealRunner
            .run(&Cmd::new("yes", Duration::from_millis(300)))
            .unwrap();
        assert!(out.timed_out);
        assert!(!out.success());
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(out.stdout.len() > 65_536);
    }

    #[test]
    fn group_kill_reaches_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");
        let script = format!("(sleep 2; touch '{}') & wait", marker.display());
        let start = Instant::now();
        let out = RealRunner
            .run(
                &Cmd::new("sh", Duration::from_millis(300))
                    .args(["-c", &script])
                    .own_group(),
            )
            .unwrap();
        assert!(out.timed_out);
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2300));
        assert!(!marker.exists(), "grandchild outlived the group kill");
    }
}
