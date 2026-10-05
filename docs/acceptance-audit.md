# Acceptance inventory at abb8f98

This is an evidence map, not a completion certificate. The original fourteen
IMPLEMENTATION.md deliverables and nine design acceptance groups remain the scope.
No authenticated native execution or actual remote host has been tested here.

## Deliverables

| Item | Inspected evidence and current boundary |
| --- | --- |
| D1 Projects adaptation and isolation | NOTICE pins upstream MIT attribution; herdr-plugin.toml uses the isolated binary and managed new action. Fresh installed-Herdr manifest/resource acceptance passed with the abb8f98 executable and manifest; startup hooks remained disabled. |
| D2 Inherited correctness and Nix portability | coordinator_review.rs checks both projects on one remote machine, unknown branch on timeout, corrupt inventory refusal. ticker.rs retains per-project machine scheduling and checked inventory. Package wraps required tools and rewrites manifest paths. SSH/Herdr here are fixtures; actual remote acceptance remains open. |
| D3 SQLite authority/DAG/capacity | tests.rs exercises two simultaneous dispatches, one reservation, durable replay and changed-content refusal. task_amend_tests.rs verifies new contract hash/dependencies, retained audit, cycles/foreign edges, active-task fencing and dispatch races. |
| D4 Durable effects | Real JetStream offline/restart test preserves a reserved job once and refuses ACK on actual SQLITE_FULL. recovery.rs blocks cancellation release on unresolved worker, coordinator, checkout, check or CAS effects. Lost native effects additionally need the actual runtime matrix. |
| D5 Identity/enrollment/routes | identity.rs defines owner/author/recipient/epoch/expiry and signed hashed records with private Ed25519 keys. Service-setup regressions cover isolated enrollment, lost token publication, changed grants and authority collisions. Actual remote key/grant transfer remains untested. |
| D6 NATS | Real restricted broker tests reject unauthorized node publishing and preserve messages across offline delivery and failed persistence. This establishes local real broker behavior, not NetBird transport or actual host outages. |
| D7 Native executor and functional readiness | Three-launcher tests cover exact sessions/bootstrap using simulated runtimes. Fresh abb8f98 Herdr resource acceptance starts no models. Authenticated coordinator/author/reviewer workflows for Codex, Claude and Pi are required and unproven. |
| D8 Mailer extension | Independent commit 7a12d5a/package evidence is recorded in docs/validation.md. No new independent mailer build was performed in this audit. It must be the protocol-2 implementation selected by live acceptance. |
| D9 Scoped API/CLI/MCP and lifecycle | Private IPC/MCP and scoped worker tests, plus questions regressions, cover actor boundaries. lifecycle_tests.rs uses real owned harmless child processes to test cancellation/deadline/exit accounting; model behavior is not inferred. |
| D10 Results/review/Git/cleanup | verification_tests.rs covers exact combined candidate for SHA-1/SHA-256, checks, independent review, lying author, CAS recovery, conflict and changed base. Worker/verifier/reviewer/coordinator retirement is implemented; missing exit evidence deliberately retains resources. Authenticated reviewer and real remote commit transport remain open. |
| D11 Curated memory | live_memory_tests.rs checks selected topics, transactional outbox, preserved assignment snapshot, revision/withdrawal ordering, scoped explicit ack and native hint identity. Its acknowledgement explicitly reports model_behavior_verified=false. Actual model read/ack remains open. |
| D12 UI authority | managed_tests.rs verifies controller projections, exact revisions, legacy fencing, scoped focus and memory actions. Paginated/record tests cover bounded IPC/MCP reads and changed content. Actual remote focus and authenticated interactive workflow remain open. |
| D13 Nix/package/docs | Blueprint prefix=nix, locked Cargo/flake inputs, package manifest/resources, service module evaluations and pinned CI are present. abb8f98 x86_64-linux build/check passed with 638 unit/22 CLI tests. No host activation or CI remote run is claimed. |
| D14 Acceptance evidence | Local suite at abb8f98 predecessor working source: 638 unit/22 CLI, strict Clippy, formatting; three actual plan-only driver commands. Real model, restart-boundary and remote-host acceptance is not supplied by those results. |

## Original design acceptance groups

| Group | Existing evidence | Unproven scope |
| --- | --- | --- |
| Native Herdr | Real isolated resources/replay/retirement; protocol fixtures cover identity failures and watcher reconnect/resnapshot | Actual model-bearing resource recovery and remote endpoint behavior |
| Scheduler | Transactional simultaneous claims, dependency/cycle checks, reviewer reservation, immutable active contracts and amendment races | Behavior of an authenticated coordinator exercising these APIs |
| Identity | Signed records, retired credentials/generations, exact native UUID/epoch and conflicting parent-environment fixtures | Complete installed runtime chain with real credentials |
| Mailer | Frozen operation and recipient preconditions, duplicate/lost-receipt fixtures; independent protocol-2 package evidence | Authenticated native notification to scoped API result |
| Persistence | SQLite pager-full, failed audit/outbox transactions, migration rollback, restart/CAS/effect receipt tests | Full named recovery-table coverage still must be matched individually; do not infer every crash boundary from a suite count |
| Git/review | Real Git combined candidates, SHA-256, changed base, conflict, dirty checkouts, failed/mutating checks and false author claims | Actual remote source transport and authenticated independent reviewer |
| Lifecycle | Real harmless process cancellation/deadline, lost-wrapper/exit accounting and cancellation recovery | Authenticated supervisor loss with live workers and end-to-end resume without duplicate spawn |
| Runtimes | Model-free launch protocol fixtures; opt-in driver prepared | Fresh authenticated Codex/Claude/Pi sessions and result-by-API, with transport/turn/result separated |
| Nix | Offline local package/check, evaluated opt-in modules, CLI/MCP/service fixtures and fresh abb8f98 installed packaged manifest/resource smoke | abb8f98 gate passed; no other-system build or deployed service claim |

## Named recovery-table cases

These map the original twelve rows to current regression evidence. Test execution
is supplied by the c646172 package log and current local suite, not by file names
alone. Native/model boundaries stay simulated unless stated explicitly.

| Original case | Evidence to retain | Boundary |
| --- | --- | --- |
| Reservation before worktree creation | `executor_reconciles_lost_creation_reply_and_fences_cancelled_work`, branch with no produced worktree | Real Git; simulated Herdr |
| Herdr created worktree, response lost | Same executor test, produced-worktree branch; installed native resource driver replay | Real Git/native resources separately; deliberate lost-response scenario uses fixture |
| Lost mailer registration before launch | `registration_recovery_node_is_durable_idempotent_and_preserves_original_assignment`, `registration_recovery_workers_resume_native_launch_after_lost_registration_response` | Receipt/launch protocol fixture, no real model |
| Runtime starts, launch response lost | `coordinator_lost_create_and_launch_replies_never_duplicate_effects`, three worker launcher tests | Uncertainty does not duplicate spawn; model simulated |
| Controller restart with active workers | Continuous CLI workflow and `controller_service_pumps_coordinator_intents_and_reconciles_after_restart` | Real services/NATS/Git/processes, native/model fixture |
| Supervisor exits or changes conversation | `rebinding_retires_old_capability_and_never_redirects_old_notice`, coordinator retirement/native-wrapper tests | Generation fencing and exact frozen destination; real authenticated replacement pending |
| Duplicate result after timeout | `worker_needs_exact_binding_then_result_is_not_an_approval_or_free_slot`, scoped worker replay tests | SQLite operation/hash contract; actual model timeout not injected |
| Retired worker result | `cancellation_fences_workers_without_freeing_unknown_processes`, scoped worker revocation | Capability/epoch checks; no late real-model message |
| Mailer send succeeded before SQL receipt | `lost_mailer_response_or_sql_receipt_reuses_exact_target_and_frozen_body` | Frozen sender/recipient/body/idempotency key, simulated mailer |
| Git CAS before SQLite confirmation | `git_cas_after_sql_failure_is_reconciled_once_without_reapplying` | Real Git ref advanced, SQL trigger fails, reopened store reconciles once |
| Herdr restart or endpoint change | `watchers_reconnect_resnapshot_coalesce_and_close_owned_streams`, `changed_targets_abort_old_stream_and_limits_are_explicit` | Real Unix framing with protocol fixture; actual model-bearing Herdr restart pending |
| Budget/deadline exhausted | `deadline_stops_native_process_without_a_live_controller_and_controller_expires_run`, owned-child cancellation tests | Real harmless child; unknown exit remains reserved |

The table is a recovery coverage map, not a claim that the remaining live matrix
has passed. In particular, a private socket fixture cannot certify that installed
Herdr or a provider processes an event as expected across a real restart.

## Reconcile historical backlog without changing scope

The historical `docs/validation.md` “Still required” section mixed work and
extensions. Artifact-input and coordinator retirement now have implementations
and completed package evidence. Planned-task amendment and stopped-attempt repair
exist; active-contract reuse is forbidden and tested. The design explicitly says
unknown outcomes retain capacity/resources. Forced verifier abandonment, killing
unattributable orphans, pruning retained original artifacts and project/cross-run
memory are not implied completion requirements. They must not be silently added
just to remove a documented conservative limit.

This does not waive the design recovery table. Each named crash/race scenario
must retain exact intent/receipt evidence and be checked against its required
scope. The authenticated runtime and real remote acceptance gaps cannot be closed
by any of the local fixture results above.

## Next dependency

Await the pending user response authorizing the fresh native participants and
identifying a NetBird host/user. Current AGENTS.md prohibits additional workers.
Do not treat the pending question or a plan's --execute flag as authorization.
Pinned build/check at abb8f98 completed successfully (638 unit tests, 22 CLI
checks). Logs and the exact output path are recorded in [validation.md](validation.md).
The packaged resource smoke also passed on isolated Herdr 0.9.1/protocol 22.
No new model or remote connection has run. The authorization question remains
pending; elapsed time and automatic goal continuations are not approval.
