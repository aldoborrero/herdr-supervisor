# Implementation contract

User goal: implement the Herdr supervisor plugin discussed with Projects adaptation, native mailer, NATS coordination, ATProto-inspired identity/records and curated memory. Start local without limiting the topology to one machine.

## Required deliverables

- [ ] Reuse Projects CLI/UI, profiles and Herdr resource workflows under an isolated binary/config/state namespace. Preserve attribution.
- [ ] Fix remote per-project polling, unknown remote branch state, corrupt inventory and Nix portability.
- [ ] SQLite authority: runs, versioned tasks/DAG, attempts, role-scoped principals, revisions, journal, transactional capacity and operation idempotency.
- [ ] Durable effects: outbox, node inbox, reconciliation after lost replies, uncertainty and epoch fencing; no duplicate worktrees or launches on replay.
- [ ] Private node enrollment, signed versioned records with owner DID and delegated host keys; no owner private-key distribution; validated routes.
- [ ] Real NATS JetStream controller/node protocol, ACK after persistence, reconnect/offline delivery, per-node ACL example and authenticated tests.
- [ ] Herdr executor with exact resource/session identity, safe launch of Codex/Claude/Pi, local mailer registration and native delivery; functional worker readiness.
- [x] Mailer client operation key and exact recipient preconditions as needed, tested independently; separate mailer commit (`7a12d5a`, Nix flake check passed on x86_64-linux).
- [ ] Supervisor/worker API and CLI (plus MCP facade) with scoped capabilities; pause/resume/cancel and recovery.
- [ ] Immutable results, independent review/check receipts for exact SHA, Git CAS integration on an owned branch, conservative cleanup.
- [ ] Memory proposals/approval, provenance/revision/withdrawal, task-specific bounded snapshots and NATS change events.
- [ ] Projects UI projections and plugin actions use the same authority; managed projects cannot bypass the controller through legacy commands.
- [ ] Blueprint flake, locked deps, package/plugin resources, devshell, formatter, opt-in NixOS/Home Manager service modules, CI, README and operations guide.
- [ ] Requirement-by-requirement test evidence, offline Nix build/check, isolated runtime tests and honest distinction between live and mocked model processing.

The inherited docs are architectural references, not claims of implementation. No deployment, public fork, push or host changes in this task.

## Current implementation checkpoint

Controller, signed node transport, curated run memory and scoped controller/worker API/MCP are implemented. The node executor prepares exact Git/Herdr resources, launches Codex/Claude/Pi through a one-shot wrapper, waits for the controller's resource-binding receipt and bootstraps through native mailer with exact recipient preconditions. `run-node` combines relay, worker API and executor; its full service workflow still needs integration coverage.

Code results now produce controller-owned combined candidates, independent check receipts and scoped reviewer attempts. `run-controller` combines API, transport and Git execution. Integration uses an owned ref with a persisted CAS intent and reconciles a Git update whose SQLite receipt was lost.

Native wrappers now persist spawn/exit receipts, preserve interactive terminal behavior and act on cancellation/deadlines. The node reconciles exits through signed events before releasing capacity. Explicit attempt stop, repair retry, replacement of reviewers that exited without a review, and run completion are implemented. Completion releases the repository binding. Receipt scope is the native process; lost wrappers remain uncertain. Artifact verification, broader replanning, cancelled-run binding recovery and conservative cleanup remain required; this checkpoint does not complete the lifecycle/review deliverables as a whole.

Native coordinator attachment now binds one exact mailer/native session and controller generation. Context is available over CLI/MCP; transactional notices preserve their recipient and frozen body through replay. Context drains unacknowledged notices before history, and explicit actor acknowledgements remain separate from delivery. Automatic coordinator launch now reserves an owner-authorized generation, creates a private Herdr workspace, claims one wrapper, binds the observed exact session and queues bootstrap. The controller pumps launch intents independently of checks/notices; retired owned processes produce durable exit receipts before replacement. Lost registrations/wrappers remain unresolved. Aggregate snapshot/wire bounds remain pending.

Projects can now attach a fresh folder to a controller run. Its list/context/overview/picker/popup project tasks, attempts, notices and curated memory from the API; lifecycle/memory actions validate run scope and operation replay transactionally. Legacy loaders/ticker/deferred rename notifications are fenced, and an authority lease excludes mode transitions during existing operations. Open can submit a launch from an explicit owner configuration, report a pending intent or focus the observed exact native coordinator. Full onboarding, profile/task forms, cross-host resource focus and asynchronous view refresh still need implementation; the full UI/onboarding deliverable remains open.

Opt-in NixOS and Home Manager modules now expose the controller and named execution nodes, with explicit runtime configuration paths, private file creation masks, owner/state validation and graceful SIGINT shutdown. Their 36 Linux checks evaluate real NixOS/Home Manager modules and rendered units; aarch64-linux and the Darwin rejection case also evaluate. No host activation has been performed. Full package/flake checks for this module checkpoint are pending: the attempt failed during Rust dependency staging because the shared filesystem ran out of space. Complete onboarding and broader service-loop coverage remain pending.

Current suite: 398 unit tests and 6 CLI tests pass; the runtime fixtures execute harmless stub processes, not authenticated models. Herdr/mailer launch boundaries are simulated; Git, Unix IPC, process signals, a pseudoterminal and dedicated broker tests are real. The controller service and signed reviewer round trip have real-broker coverage. See docs/validation.md for specific evidence and unresolved requirements. The unchecked deliverables retain the full original scope.
