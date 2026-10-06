# Local native acceptance, 2026-10-06

The user authorized a process-compose environment and fresh isolated participants.
Both complete local Codex and Claude acceptance runs passed. Pi model acceptance
is explicitly deferred pending `/login`; its selected authentication cache was
empty. NetBird/SSH and actual remote-host testing are deferred separately.

## What actually ran

The lab used real NATS JetStream with separate ACL credentials, herdr-mailer
protocol 2, Herdr, controller, node, SQLite, Git and provider-authenticated models.
Each run created one supervisor, an author in an isolated worktree and an
independent reviewer. The driver supplied the challenge and observed results; it
did not submit participant readiness, code results or review decisions.

Both runs completed the exact candidate/check/review/integration contract, a
scoped question and answer, curated memory approval/read/acknowledgement and
recorded native exits. This is evidence of model processing, beyond a broker ACK
or native queue receipt. The checked `down` command subsequently stopped each
lab's services. Private worktrees, databases and evidence remain retained.

| Runtime | Run ID | Integrated candidate |
| --- | --- | --- |
| Codex | `d92b4b1c-0baa-4b7b-846a-b11b87b7110d` | `bd6f25692ca29149297afa6b709f6cef3dfb0721` |
| Claude | `785ea14a-f3bc-45d6-8f17-5de268f7e02d` | `6264315c6bba199dec5910fca8581a13bada82cd` |

Codex used 0.158.0 with `gpt-6-astra`. Claude used 2.1.284. Both used Herdr 0.9.1,
protocol 22, NATS 2.14.7 and Process Compose 1.122.0. Mailer was built from
`7a12d5a`; the selected package was
`/nix/store/i091vmb8gamhvnn0m9kva1gpmrn67b6g-herdr-mailer-0.1.0`.
Executables were pinned explicitly; personal wrappers that fetch versions or
load host secrets were excluded.

Codex ran the debug supervisor built from the Rust source committed as `8a39571`.
Claude ran that commit's Nix package:
`/nix/store/qada5wwl8rzyh9iw2ww5d26zk72m5375-herdr-supervisor-0.1.0`.
The laboratory helpers additionally contained the persisted workspace/hook trust
and explicit Claude wrapper refinements committed after that package. The later
Pi identity change has automated coverage, not an authenticated Pi model run.

Local evidence, intentionally not committed because lab state is private:

| Runtime | Retained root | Worktree log |
| --- | --- | --- |
| Codex | `/tmp/herdr-supervisor-live.3pzisip0` | `.direnv/local-codex-acceptance-hook.log` |
| Claude | `/tmp/herdr-supervisor-live.vfonzncc` | `.direnv/local-claude-pinned-acceptance.log` |
| Pi, incomplete | `/tmp/herdr-supervisor-live.llu8jovj` | Private root logs; model unavailable |

Successful roots contain `acceptance-evidence.json` with `status: passed`, three
native session IDs, `question_resolved: true`, `memory_acknowledged: true`, and
`cleanup.status: recorded_native_exits`. `remote_host_acceptance` remains false.
Earlier unsuccessful fresh trials were cancelled with recorded exits before
their services were stopped; their private state was retained too.

## Findings and corrections

- Codex Auto cannot be combined with the older explicit sandbox flag. Its
  official Herdr session hook must be installed and trusted, and a neutral first
  prompt creates the native thread before its UUID can receive the bootstrap.
- The installed Codex TUI requires persisted trust for its exact workspace. The
  lab writes this only inside its private configuration and verifies the reviewed
  official hook hash through the real app-server. No trust bypass is used.
- Claude runs with Auto and exact private workspace trust. Its local development
  channel still required a separate confirmation for the supervisor, author and
  reviewer. These three prompts were accepted explicitly in the fresh panes;
  no task/result was injected to help the models. This first launch is therefore
  not fully unattended.
- Pi 0.87.1's official Herdr integration reports a transcript path. The managed
  runtime now resolves the UUID from a bounded, owned regular-file session header
  matching the workspace, never from its filename. Missing headers fail closed.
  A neutral first prompt requests session initialization before bootstrap.
  Coordinator and worker fixtures exercise path-based reports. Remote path-based
  Pi focus remains unsupported rather than reading a remote path locally.
- Pi's authenticated workflow is not tested: its auth cache has no provider, and
  the user chose to leave it pending `/login`. No credentials were invented or
  copied from a different provider/runtime.

## Reproducible checks and limits

Use [the local lab guide](local-lab.md) to prepare and run a fresh environment.
No host activation, global configuration edit, existing-session modification,
push or deployment was performed.

The `8a39571` x86_64-linux package build and flake check passed with 638 unit
tests, 22 CLI tests and formatting. Logs are
`.direnv/local-lab-nix-build.log` and `.direnv/local-lab-nix-check.log`.
Clippy ran separately. Nix's Git fetcher rejected a missing historical object in
the shared checkout although Git could read and archive HEAD. The package gate
therefore used an exact `git archive HEAD` export and `path:` flake reference;
no ignored lab data entered that export and shared Git metadata was not repaired.

Final follow-up suite/package evidence is recorded in `docs/validation.md`.
The tests cover the local happy path and their named regression boundaries.
They do not establish the full live restart/recovery matrix, real remote-host
operation, other-system builds or completion of all supervisor deliverables.
