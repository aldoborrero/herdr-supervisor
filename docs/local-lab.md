# Local integration lab with process-compose

The repository's `process-compose.yaml` runs real NATS JetStream, herdr-mailer,
Herdr, the supervisor controller and one execution node on this machine. No
Docker daemon, NetBird or SSH is needed. Models are started only by the separate
`test` command. The [recorded local results](local-lab-evidence.md) cover complete
Codex and Claude workflows; Pi model acceptance awaits `/login`.

Use `nix develop` for Python, process-compose, NATS and the Git tools. Build the
supervisor and select a protocol-2 mailer package with idempotent sends and exact
recipient preconditions. Herdr and the native CLIs remain explicit local inputs.
Select the actual packaged executables, not personal wrappers that fetch updates
or load host secrets. The helper does not install these tools. For `--runtime pi`,
pass `--pi /absolute/package/bin/pi`; for Claude, pass `--claude /absolute/package/bin/claude`. These paths are fixed through private PATH entries.

Preparation installs the selected runtime's built-in Herdr integration into the
private lab home. That integration supplies the native session UUID; detecting a
terminal as Codex/Claude/Pi is not enough to route messages. For Codex, the helper
checks the reviewed version-8 script's SHA-256, asks the real app-server for its
hook metadata/hash, persists that one hook's trust locally and verifies it. A
changed script fails preparation for review. It never uses a hook-trust bypass.

```sh
nix develop
python3 dev/local_stack.py prepare \
  --root /tmp/herdr-supervisor-live.codex-trial \
  --supervisor /absolute/package/bin/herdr-supervisor \
  --mailer /absolute/mailer-package/bin/herdr-mailer \
  --herdr "$(command -v herdr)" \
  --codex "$(command -v codex)" \
  --nats "$(command -v nats-server)" \
  --compose "$(command -v process-compose)" \
  --runtime codex
```

The root must be new. Omit `--root` to allocate a private temporary directory and
read its path from the returned JSON. Preparation creates one baseline Git
commit, private controller/mailer identities, random broker passwords and the
configuration files; it starts no service or model. Use separate roots for
`codex`, `claude` and `pi`. Optional `--model NAME` pins the profile model and
`--duration SECONDS` selects a 120–1800 second run deadline (default 600).

For an authorized authenticated trial, add `--use-local-auth --auth-home /home/USER`
when preparing. This copies only the selected runtime's authentication cache into
private lab state: `.codex/auth.json`, `.claude/.credentials.json` or
`.pi/agent/auth.json`. It does not copy transcripts, plugins or global settings,
print credentials or write back to the original cache. Claude also preserves the
already-completed onboarding flag (when present) and uses a local dark theme; no
other global preferences or project permissions are copied. Without this explicit
option, authenticate the runtime in its isolated lab environment separately.
An existing cache may have expired or may use an unsupported credential store;
the actual runtime determines whether authentication succeeds. Treat the whole
lab root as private, including after services stop.

Start services in one terminal, then check them from another:

```sh
python3 dev/local_stack.py up --root /tmp/herdr-supervisor-live.codex-trial
python3 dev/local_stack.py status --root /tmp/herdr-supervisor-live.codex-trial
python3 dev/local_stack.py check --root /tmp/herdr-supervisor-live.codex-trial
```

NATS listens only on a newly selected loopback port, with separate administrator,
controller and node credentials and the supervisor's exported ACLs. Its 2 GiB
file-store quota permits the supervisor stream's reservation; this is a cap, not
a preallocated data file. The mailer uses local SQLite/native delivery here;
supervisor coordination uses actual NATS. Process Compose listens on a private
Unix socket. No default TCP management port is opened.

The dependency graph waits for NATS, mailer and Herdr before registering the
notifier and provisioning the local bundle. Controller and node start only after
that bootstrap succeeds. Restarting services reuses the existing identities,
bundle and persisted broker state. It does not intentionally create a new run or
rotate registrations. A failed bootstrap stays visibly failed; inspect the
private `logs/` directory before retrying.

The Claude lab wrapper selects Auto, persists trust for only its exact local
workspace and retains the runtime's permission review. Claude 2.1.284 separately
asks to confirm the local development channel for each new participant; accept
that prompt only for this reviewed local mailer channel. The helper does not
bypass it. See [Claude permission modes](https://code.claude.com/docs/en/permission-modes).

All services receive isolated HOME/XDG/Codex/Claude/Pi directories. Parent thread
IDs, mailer configuration and supervisor capabilities are not inherited. Herdr
uses a named private server, no login shell and disabled update/resume hooks.
`check` validates real service health, not provider authentication or model work.

The lab's Codex wrapper persists `projects.<path>.trust_level` in the lab's private
`CODEX_HOME` for the exact `--cd` directory, after checking it is inside this lab.
This prepares the authorized fresh coordinator/worktrees without changing the
user's trust settings or Codex Auto permissions. Queue/status commands go to the
same real binary without writing trust. The installed TUI requires persisted
trust; a command-line override alone did not satisfy onboarding. See the [official configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference).

## Run the native acceptance task

The user authorized these new isolated participants on 2026-10-06. In a different
execution, obtain the relevant model/session authorization before using `test`.
This command can consume provider quota:

```sh
python3 dev/local_stack.py test --root /tmp/herdr-supervisor-live.codex-trial
```

It runs [native acceptance](live-acceptance.md): one supervisor, an author and an
independent reviewer, one file with a random challenge, an exact check and Git
integration, a scoped question, curated memory and confirmed exits. The driver
never fabricates model results. A coordinator that exits early fails promptly
instead of waiting until the deadline. A failed/completed acceptance root is
retained; use a fresh root for another run rather than deleting uncertain state.

After the run has stopped, shut down services:

```sh
python3 dev/local_stack.py down --root /tmp/herdr-supervisor-live.codex-trial
```

The helper refuses shutdown while an attempt is active or a coordinator's exit is
unresolved. Cancel through the supervisor and reconcile exits first. Ordered
shutdown sends SIGINT with a 30-second timeout. Worktrees, logs, credentials and
databases remain in the private root for review; `down` does not delete them.
Abrupt external termination of Process Compose is not equivalent to this checked
shutdown path.

The orchestration follows [Process Compose dependencies and shutdown](https://f1bonacc1.github.io/process-compose/launcher/).
Codex cache reuse follows the [official authentication documentation](https://learn.chatgpt.com/docs/auth).
Neither dependency readiness nor a native queue receipt proves model processing.
