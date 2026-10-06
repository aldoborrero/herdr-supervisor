# Native model acceptance

`tests/live_acceptance.py` drives a code task through already prepared, isolated
real services. Its default mode only prints a plan. Execution starts real model
sessions and can incur provider charges; it is separate from offline package
tests and from the model-free native resource test.

**Current evidence (2026-10-06):** complete local Codex and Claude model runs
passed with real services, scoped model results and independent review. Pi model
acceptance is deferred by the user pending `/login`. Claude required the local
development-channel confirmation for each new participant. See the exact
[evidence and limitations](local-lab-evidence.md); a plan or service health check
alone does not establish model processing.

## Prepare and review

For a reproducible local environment, use the [process-compose lab](local-lab.md).
It supplies the service preparation described below without NetBird or SSH.

Use a fresh private root for each runtime, named
`/tmp/herdr-supervisor-live.NAME`, with these prepared paths:

| Path | Required state |
| --- | --- |
| `controller/` | Private initialized controller, no runs, exactly one active node |
| `node/` | Private enrolled node, no jobs |
| `services/` | Unmodified local bundle, including `intent.json` and `launch.json` |
| `repo/` | Clean new Git repository, exactly one baseline commit, no `acceptance.txt` |

Follow [local service setup](getting-started.md#prepare-a-local-service-bundle).
The repository map must use the absolute `ROOT/repo` path as both key and value.
Keep the worktree root, private Herdr socket, mailer state and mailer admin
credential inside the root. Retain the setup command's separation rules for
service/repository directories. Private state directories need mode 0700, their
files mode 0600, and ownership by the current user. Symlinked paths are rejected.

Prepare dedicated Herdr, mailer and restricted NATS services, then start the
controller/node using the bundle's generated argv. Herdr must have no agents and
every pane's working directory must be inside the root. Use fresh HOME/XDG state
for these services, without selecting shared servers or existing user sessions.
Local path and health checks cannot prove isolation of every external broker or
provider selected by an operator.

Authenticate the runtime for these isolated services separately through its
supported setup. The driver does not read or copy provider credentials. The
native launcher must be able to find that runtime. Claude also requires explicit
`claude_development_channel: true` in the frozen bundle. Dependency checks do not
verify provider authentication.

Save a specification outside the repository, replacing these illustrative paths:

```json
{
  "root": "/tmp/herdr-supervisor-live.codex-example",
  "binary": "/absolute/package/bin/herdr-supervisor",
  "runtime": "codex",
  "profile_args": [],
  "duration_secs": 600
}
```

Select `codex`, `claude` or `pi`. Optional profile arguments allow only bounded
model/tuning flag-value pairs; permission overrides and configuration switches
are rejected. Duration must be 120–1800 seconds.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tests/live_acceptance.py /private/spec.json
```

This reads the specification and prints effects without opening sockets, reading
service credentials, creating state or starting subprocesses. It does not need
services to exist and does not establish their readiness.

## Execute an authorized run

The repository's [AGENTS.md](../AGENTS.md) says: “No push, deployment, global
configuration, live user sessions, or additional workers.” An agent working under
that instruction needs an explicit exception for the fresh native coordinator,
author and reviewer. The user granted that exception for isolated local tests on
2026-10-06; do not ask for it again in this task. The command flag itself is not
authorization for an unrelated execution. Existing user sessions are never required.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 tests/live_acceptance.py /private/spec.json --execute
```

After preflight, the driver creates one run and starts one native coordinator.
That coordinator must create the task and dispatch an author and independent
reviewer. The driver does not supply readiness, results, reviews, clarifications
or memory acknowledgements on their behalf.

The author adds only `acceptance.txt` containing a fresh random challenge, asks a
scoped question, receives the coordinator's answer, proposes a memory note and
reads/acknowledges its approved revision. The coordinator must obtain the actual
check and review for the exact candidate, integrate it, stop both attempts and
complete the run.

Policy permits two active attempts, reserves a reviewer slot and allows one
author attempt per task. The driver observes once per second and fails if it sees
more than one task or two attempts. This observation is not an atomic global cap
on total launches. The run deadline is not a monetary budget; the driver does not
enforce provider spending limits.

## Evaluate and retain evidence

Success requires an integrated task, stopped author/reviewer attempts in distinct
native sessions/worktrees, a distinct coordinator session, exact passing check
and independent review receipts, and the expected Git ref/content. The journal
must attribute task creation to the scoped supervisor. The resolved question,
approved memory provenance and explicit worker memory acknowledgement must also
exist. The driver examines actual service records and Git. This establishes the
exercised workflow, not the quality of arbitrary future work.

Each mutation's token-free request is written before IPC with its operation ID;
retries reuse it. The root retains `acceptance-plan.json`, `acceptance-run.json`,
`request-*.json` and, for a known run, `acceptance-evidence.json`. Reports contain
IDs and outcome boundaries, not tokens or full provider transcripts. Preserve
private service databases/logs for independent review.

On failure/interruption, the driver requests cancellation of a known active run
and waits up to 30 seconds for recorded attempt/coordinator exits. Missing
evidence remains `unresolved`; an otherwise successful run becomes `incomplete`
without exit records. It does not force-kill processes, remove worktrees, stop
external services or infer exit from timeout. Uncertain creation without a
returned run ID retains its exact request: reconcile it before another launch.
Do not delete state to retry.

Use a new root for each runtime. Each passing report covers one runtime on one
host. `remote_host_acceptance` remains false. Remote execution, connectivity loss
and recovery need separate evidence; this driver also performs no restart fault
injection. The [design's full matrix](supervisor-design.md#plan-de-validaci%C3%B3n-de-la-implementaci%C3%B3n-futura)
remains the completion contract.

## Local driver checks

`tests/live_acceptance_test.py` covers plan-only behavior, invalid specifications,
foreign-state refusal, real Unix response framing/error redaction and evaluation
of constructed evidence. The evaluation fixture uses isolated SQLite and
simulated Git/page results: it is not a model run. CLI test
`live_acceptance_driver_tests_never_launch_models_or_use_host_services` includes
these checks in `cargo test` and the offline Nix package gate.
