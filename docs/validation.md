# Validation and completion status

Reviewed 2026-10-05. This document separates implemented/tested behavior from the full delivery contract in [IMPLEMENTATION.md](../IMPLEMENTATION.md). Passing the current suite does not establish completion of the supervisor.

## Evidence collected

The local implementation suite currently passes **422 unit tests and 7 CLI tests**, including the inherited Projects suite. The controller tests use private temporary state; real Git, SQLite, sockets, native child processes, a pseudoterminal and isolated NATS brokers are exercised. Runtime tests replace the model executables and Herdr/mailer boundaries with controlled fixtures.

| Requirement | Evidence | Limit |
| --- | --- | --- |
| Atomic capacity, DAG/revisions and operation replay | `src/control/tests.rs`: concurrent dispatch, dependency cycles, retired supervisors and transaction rollback; candidate tests schedule a reviewer while the author occupies the worker slot | Broader replanning/abandonment of an uncertain verifier pending |
| Unknown remote branch, inventory corruption and project polling | `src/coordinator_review.rs` plus inherited suite | Managed projection now tested separately; automated onboarding still pending |
| Signed origin and route authorization | Identity tampering/expiry tests, controller receive tests, real NATS ACL tests | Explicit node rotation/revocation commands remain pending |
| Reply inbox confidentiality and broker management | `real_broker_generated_acl_isolates_reply_inboxes_and_runtime_management` requires actual NATS permission errors for foreign reply subscriptions, consumer reads, stream mutations, message reads and ACKs; confirms legitimate deliveries/ACKs/reconnects | Generated runtime policy must be applied to the deployed broker; administrators remain trusted |
| Broker policy usability and reserved routes | `broker_routes_reject_reserved_names_and_wildcards_before_enrollment_or_grant`; continuous CLI workflow invokes `broker-policy` and installs its permissions on separate controller/node users | Example in `docs/nats-acl.md`; existing host/broker configuration is not changed |
| Durable offline transport | Real JetStream restart/offline test; ACK follows SQLite acceptance | Inbound poison-record handling and recovery of terminal outgoing records remain pending |
| Durable retry and lost SQL publication receipt | `broker_acceptance_with_lost_sql_receipt_retries_identical_bytes_after_restart` injects failure after real broker storage, reopens SQLite and checks unchanged wire/ID, broker dedup and controller application | Network timeout alone never establishes acceptance |
| Expired, malformed and premature outgoing records | `expired_malformed_and_future_outgoing_records_do_not_block_commands_or_receipts` keeps expired/invalid rows, defers a future timestamp and preserves active process capacity | Outgoing terminal states do not establish task outcomes |
| Fairness across denied routes | `denied_outgoing_route_does_not_starve_incoming_events_or_other_nodes` receives a worker proposal and delivers its receipt while another destination is denied by real NATS ACLs | Incoming consumer/service outages remain separate failures |
| Bounded retry schedule and private inspection | `retry_schedule_survives_restart_is_bounded_and_operator_projection_omits_credentials`; continuous CLI workflow invokes `delivery-status` | At most 100 recent metadata rows, with full state counts; no terminal requeue command |
| Retry migration and native binding lookup | `node_v4_retry_migration_preserves_wire_jobs_memory_and_idempotent_operations`; extended three-runtime bootstrap test retains a malformed event row | Node schema 5; no native model calls or grant renewal |
| One resource after a lost creation response | `executor_reconciles_lost_creation_reply_and_fences_cancelled_work` | Real Git; Herdr response/inventory simulated |
| Exact pane and session binding | `three_native_launchers_bind_exact_sessions_before_idempotent_bootstrap` | Three runtime routes, stub executables and fake Herdr observations |
| One-shot launch and credential environment | Same three-runtime test executes the actual owned wrapper in a child process, refuses a second claim and supplies conflicting parent environment | No authenticated model or live Herdr server contacted |
| Restricted runtime flags | `runtime_profiles_cannot_override_permissions_identity_or_checkout` | Version compatibility requires installed CLI support |
| Scoped worker context, mutations and receipts | `worker_boundary_scopes_context_mutations_receipts_and_revocation` | A queued event is not necessarily accepted by the controller |
| Worker Unix API and MCP facade | `worker_socket_and_mcp_expose_only_scoped_assignment` | JSON-RPC protocol tests, not an official SDK compatibility suite |
| Controller Unix API and MCP | `private_api_and_mcp_share_authorization_and_incomplete_clients_do_not_block` | Same-UID private IPC |
| Receipt replay after cancellation | Worker-boundary test: exact accepted envelope can be acknowledged; a new envelope with the retired token is refused | Node signature/epoch must still be valid |
| Monotonic receipts | Worker-boundary test sends a late publisher ACK after `controller_applied` | Model processing remains separate |
| Lease release with inherited file descriptions | `lease_release_is_not_delayed_by_an_inherited_file_description` | Explicit unlock prevents fork/exec inheritance from delaying release |
| Coordinator context and scoped acknowledgement | `coordinator_context_is_scoped_and_exposes_memory_evidence_without_secrets`, `context_drains_unacknowledged_notices_across_multiple_pages`; CLI test connects twice with one operation and reads private context | Manual attachment and scoped context; large-run pagination pending |
| Durable coordinator wakeups | `journal_cursor_and_notices_roll_back_together_and_restart_deduplicates`, `lost_mailer_response_or_sql_receipt_reuses_exact_target_and_frozen_body`, `rebinding_retires_old_capability_and_never_redirects_old_notice`, `wrong_recipient_receipt_is_unresolved_and_idle_notifier_needs_no_mailer`, `uncertain_send_cannot_be_retried_from_a_different_logical_author` | Mock mailer persistence/response loss; no model consumption claim |
| Notifier service wiring | `controller_service_delivers_coordinator_bootstrap_with_real_broker` | One-cycle service, real NATS and mocked mailer |
| Managed Projects authority and UI | `managed_projects_render_controller_records_and_decisions_use_exact_revisions`, `managed_binding_blocks_legacy_effects_and_rejects_foreign_owner_decisions`, `project_authority_transition_excludes_existing_operations_and_clones`, `failed_priming_keeps_managed_fence_and_retry_repairs_only_the_view` | Real Unix API/SQLite; legacy Herdr effects observed with a fake runner |
| Managed memory and actions | `managed_memory_actions_preserve_provenance_revision_and_withdrawal`, `managed_keys_confirm_and_submit_captured_epoch_without_legacy_actions`; extended CLI test attaches, reads, pauses/replays and rejects legacy commands | Popup key test uses the command fixture; CLI test runs the actual binary |
| Responsive managed popup and picker | `managed_popup_stays_responsive_to_keys_and_freezes_confirmation_while_socket_is_gated` uses a real Unix socket, gated replies, navigation and changed epochs; selection and picker filter survive refresh | Controller replies are protocol fixtures; explicit mutations still use synchronous CLI/API |
| Bounded asynchronous reads and binding fencing | `slow_readers_are_bounded_coalesced_and_do_not_block_another_project_or_exit`, `authority_changes_and_corrupt_markers_discard_late_snapshots` | Four in-flight reads; aggregate snapshot size/pagination remains pending |
| Stale views and reconnection | `unavailable_controller_retains_explicitly_stale_data_until_successful_retry`, `stale_managed_rows_keep_details_but_disable_decisions_and_launches` | In-memory display cache only; CLI status/context/overview remain synchronous |
| Native coordinator focus | `managed_open_focuses_only_the_exact_observed_native_coordinator` | Exact UUID/runtime/terminal checked against simulated Herdr; no model launch |
| Curated memory | `memory_is_curated_versioned_scoped_and_budgeted` | Run scope; cross-run project memory pending |
| Atomic live memory and declared consumption | `live_memory_outbox_is_atomic_selected_and_retains_assignment_snapshot` injects an outbox failure, checks topic selection, preserves initial memory and rejects wrong credential/epoch or stale acknowledgement | Acknowledgement is node-local and does not prove model behavior |
| Reordered updates and withdrawal | `reordered_memory_withdrawal_replay_and_cancellation_never_resurrect_topics`, `memory_rejects_foreign_topics_hashes_budget_run_and_future_epoch` | Owner-signed commands, SQLite persistence; no federation |
| Memory migration and dispatch version | `memory_versions_fence_updates_older_than_dispatch_and_migrate_v3_without_changing_jobs` | Old unversioned assignments remain version 0 until an update; jobs and startup content preserved |
| Native memory hint replay | `native_memory_notice_replays_frozen_identity_after_bad_or_lost_receipt_and_retires_on_cancel` checks protocol capability, invalid recipient receipt, SQL receipt loss, node restart, changed sender token, identical replay and cancellation | Mock mailer IPC; its send receipt never acknowledges memory |
| Live memory through continuous services | `tests/service_workflow.py` approves memory while the author runs, withdraws it while the node is offline, broadcasts to author/reviewer and invokes the real acknowledgement CLI | Restricted real NATS, actual node/controller/wrappers, simulated native delivery/model processing |
| CLI bootstrap usability | `managed_control_help_and_bootstrap_are_usable_without_herdr` | Does not deploy services |
| Combined candidate, independent review and owned Git CAS | `combined_candidate_review_and_checks_gate_owned_branch_for_sha1_and_sha256` uses real Git repositories, a divergent accepted base and an untouched dirty user checkout | Model review content simulated; code contracts only |
| Conflicts and check evidence | `conflicts_are_explicit_and_do_not_change_either_checkout`, `failing_or_mutating_checks_cannot_be_replaced_by_an_author_claim`, `started_check_is_not_rerun_after_unknown_execution_outcome` | Repair/replanning needs explicit lifecycle support |
| Git effect followed by SQLite failure | `git_cas_after_sql_failure_is_reconciled_once_without_reapplying` injects SQL failure after real Git CAS, reopens the store and verifies one final receipt | Owned refs must remain reserved from independent Git writers |
| Ref protection | `checked_out_integration_branch_and_changed_base_are_never_overwritten` | Git clients outside the controller do not participate in its lock |
| Remote result import and ancestry | `remote_result_requires_configured_node_source_and_exact_ancestry` clones a separate repository and imports only from the configured node source | Local Git transport; SSH credentials/connectivity are deployment configuration |
| Schema migration | `migration_preserves_v2_authority_credentials_and_operation_replays` retains live v2 rows, signed outbox and operation replay across migration | No downgrade supported |
| Controller service | `continuous_controller_verifies_without_blocking_api_and_applies_owned_ref` runs the service with real NATS and Git; a gated check keeps running while a Unix API snapshot succeeds | One-cycle service mode; no authenticated models |
| Continuous controller/node workflow | CLI test `continuous_services_deliver_review_integrate_and_stop_across_node_restart` runs `tests/service_workflow.py`: offline dispatch, exact native binding before bootstrap, node restart, worker result, controller checks, a separate reviewer, owned-ref CAS, native exits, capacity release and completion | Real services/Git/IPC/restricted NATS; Codex process and Herdr/mailer are fixtures. No direct writes to service databases; no live models |
| SIGINT during node execution | The same test gates the mailer response, sends SIGINT while the executor is blocked, releases the call and requires clean shutdown/socket removal before restart | Fails on the packaged `d416dfb` binary with a shutdown timeout; passes with persistent signal receivers. Checks that worktrees, wrappers and registrations are not duplicated |
| Reviewer transport | `assigned_reviewer_uses_signed_node_transport_and_receives_controller_receipt` uses a restricted NATS node account and the worker boundary | The review decision is simulated; broker ACK cannot approve it |
| Native exit and durable capacity release | `normal_exit_receipt_survives_restart_and_loss_between_event_and_job_update` waits for a real child, restarts node state and injects failure after event enqueue | Native-process scope, not a detached-descendant or remote-provider audit |
| Cancellation and deadlines | `cancellation_stops_owned_child_and_fences_workers_before_releasing_capacity`, `deadline_stops_native_process_without_a_live_controller_and_controller_expires_run` | Real harmless children, including TERM refusal and KILL escalation; no model sessions |
| Missing receipt / stale PID | `missing_exit_receipt_never_signals_a_saved_pid_or_frees_capacity` points the stored PID at a separate live test process | Unknown wrapper outcomes remain occupied; no automatic orphan recovery |
| Prelaunch cancellation and wrapper failure | `cancellation_before_launch_is_provable_and_late_execute_stays_fenced`, `spawn_failure_and_wrapper_signal_have_durable_terminal_receipts` | No automatic pane or worktree deletion |
| Interactive terminal | `interactive_native_child_gets_foreground_terminal_and_wrapper_restores_it` creates a real PTY and sends input to the foreground native child | Linux host evidence; no live Herdr/model session |
| Node schema migration | `node_v2_migration_preserves_pending_events_and_does_not_invent_process_receipts` | Old launcher instances remain uncertain without new exit receipts |
| Repair and reviewer replacement | `stopped_reviewer_can_be_replaced_and_rejected_task_retries_with_new_evidence` | Does not abandon in-flight check or Git intents |
| Run completion | Combined-candidate test refuses completion while processes remain, then completes, replays and releases the repository binding; `successful_later_candidate_does_not_hide_an_earlier_check_with_unknown_exit` retains the reservation | Uncertain verifier abandonment remains pending |
| Cancelled repository reuse | `cancelled_repository_releases_only_after_exit_and_reuses_without_deleting_evidence` verifies exit/lease fencing, transaction rollback on lost journal receipt, idempotent release, retained Git refs/checkouts and a new run using the same canonical repository | No resource deletion |
| Uncertain cancelled effects | `cancelled_recovery_keeps_ambiguous_checks_checkout_preparation_and_owned_coordinator_blocked`, `cancelled_git_cas_requires_exact_receipt_reconciliation_before_repository_release` | No timeout/PID-based inference or forced abandonment |
| Cancellation while checking | `cancellation_during_check_drains_receipt_before_releasing_repository` gates an actual shell check, cancels while it runs and releases only after its receipt | Process receipt scope, not a detached-descendant audit |
| Cancelled intent and automatic service recovery | `cancelled_verification_that_never_started_retires_without_claiming_git_effects`, `controller_service_reconciles_cancelled_repository_and_retains_its_refs` | Real Git and private NATS service cycle; worker exit evidence is a scoped fixture |

## Owned coordinator launch

| Invariant | Evidence | Limit |
| --- | --- | --- |
| Owner authorization, replay and no unresolved replacement | `coordinator_reservation_is_owner_only_replayed_and_rejects_unresolved_replacement` | Run-authorized profile; no legacy profile imports |
| API contention and relative state paths | `coordinator_owner_decision_is_retryable_without_blocking_during_launch_effect`, `relative_controller_state_keeps_absolute_coordinator_home_and_capability_paths` | Busy mutations have no effects; retry the identical operation after the launch lease is released |
| Lost Herdr replies do not duplicate effects | `coordinator_lost_create_and_launch_replies_never_duplicate_effects` | Simulated Herdr; uncertain absence needs reconciliation |
| Lost registration does not rotate a receiver | `coordinator_lost_registration_stays_uncertain_without_rotating_mailer_session` | Simulated mailer; automatic registration recovery remains pending |
| Native binding, process ownership and generation retirement | `three_coordinator_runtimes_bind_exact_native_sessions_bootstrap_and_stop_on_retirement` | Real harmless child processes and Unix IPC, mocked Herdr/mailer; no authenticated models |
| Late wrapper is fenced after cancellation | `fenced_unclaimed_coordinator_cannot_start_late` | No process spawned for the retired generation |
| Schema migration preserves prior authority | `schema_four_migration_preserves_active_coordinator_routes_and_memory` | SQLite schema 4 to 5; routes, notices and approved memory retained |
| Service restart reconciles launch intents | `controller_service_pumps_coordinator_intents_and_reconciles_after_restart` | Real NATS and stub Herdr executable; two service cycles, one create attempt |
| Projects submits once and displays pending state | `managed_open_reserves_once_and_displays_pending_launch_without_legacy_effects` | Real controller API; owner launch configuration required |

## Nix service modules

`checks.<system>.service-modules` evaluates the actual exported NixOS and Home Manager modules with the pinned inputs. Linux checks cover disabled imports, the publishing package default, explicit non-root ownership, controller/node commands, multiple nodes, distinct normalized state, absent runtime configuration files, optional coordinator notifications, dependencies, private masks, restart limits, SIGINT/drain behavior and literal systemd argument escaping. It also reads the rendered unit files from both module systems. Darwin checks disabled imports and rejection of enabled Linux services. These are evaluation/rendering checks; no existing host or user manager is activated. See [services](services.md).

The service-module checkpoint passes 36 Linux assertions and rendered-unit checks, plus evaluation for aarch64-linux and the Darwin rejection case. The cancellation-recovery checkpoint (`1a5afe9`) passes full `nix flake check` on x86_64-linux, including package compilation and its 410 unit/7 CLI tests, the service-module checks and the formatter check. This closes the earlier disk-full validation backlog for the modules, continuous workflow, live memory and cancellation recovery. The asynchronous-view checkpoint adds five regressions listed above and uses the same full package gates. This does not establish builds on the other declared platforms.

The same source also passes `cargo test --locked --offline` and strict all-target Clippy using a task-local RAM-backed Cargo cache. Earlier `nix flake check --no-build` results established evaluation only; they have now been supplemented by the full package gate. The successful Nix build uses the normal daemon build directory. No shared permissions or daemon settings were changed.

The asynchronous-view commit (`62baee8`) passes the 415 unit/7 CLI tests, strict all-target Clippy and `nix fmt` locally. Its full Nix build compiled the application but failed while compiling the check binaries with `rustc-LLVM ERROR: IO failure on output stream: No space left on device`. The chained flake check did not run. Full package validation for that newer source remains pending; `result` still points to the previously verified cancellation-recovery package.

The inbox-ACL commit (`0cd387b`) also passed its 417 unit/7 CLI tests, strict Clippy and formatting locally. Retrying its Nix build with one compilation job reached the check phase but again exhausted the shared filesystem. The subsequent outgoing-retry source passes 422 unit/7 CLI tests and strict Clippy using the RAM-backed Cargo cache. Full Nix package/check validation remains pending for these newer changes; the successful older package is not evidence for them.

The transport audit reproduced a confidentiality gap in the old broker fixtures: their `_INBOX.>` subscription permission let a restricted node observe another consumer's JetStream pull response, even though that node could not issue the other consumer's pull request. The reproduction used a temporary loopback NATS server and fictional assignment/credential data, then stopped its owned process. The implementation now assigns distinct reply prefixes and generates the corresponding subscription ACLs. Real-broker regressions require explicit permission errors and absence of leaked payloads, while the continuous workflow exercises generated runtime permissions end to end. Applying those permissions to a deployed broker is an operator step; no host or shared server has been changed.

## Commands

```bash
NATS_SERVER=/absolute/path/to/nats-server cargo test --locked --offline
nix develop --command cargo clippy --locked --offline --all-targets -- -D warnings
nix fmt
nix build .#herdr-supervisor --cores 2 --max-jobs 1 --print-build-logs
nix flake check --cores 2 --max-jobs 1 --print-build-logs
```

The CLI suite requires Python 3 as well as NATS; both are included in the devshell and Nix check inputs. To run the continuous workflow separately:

```bash
python3 tests/service_workflow.py --binary target/debug/herdr-supervisor --nats-server "$NATS_SERVER"
```

All fixture services and worktrees are private and temporary. The test starts actual controller/node processes without `--once`, uses only their authenticated APIs for mutations, and stops its owned children on success or failure. Native submission is explicitly checked to leave the attempt awaiting readiness until the simulated worker reports it.

The Nix build of the controller/transport foundation passed on `x86_64-linux`. Package and flake checks are required again after executor changes; the local implementation commit records the completed gates for its snapshot. Some build attempts exhausted the shared filesystem, including one after the local suite passed. Only generated task compilation caches were removed manually; unrelated temporary directories and system-wide garbage collection were not touched.

Mailer protocol-2 support is an independent local commit **`7a12d5a`** on `feat/supervisor-delivery`, based on `939cff1`. `nix flake check` passes for that commit on `x86_64-linux`; the package runs Go tests, race, vet, real NATS tests and native bridge SDK tests offline. Supervisor sends require its operation-ID and recipient-precondition capabilities.

## Still required

- Artifact verification without a synthetic code commit; broader replanning and abandonment of an uncertain verifier.
- Recovery/abandonment of uncertain cancelled-run effects and conservative verification checkout cleanup.
- Recovery after loss of the native wrapper; detached-descendant audit and conservative resource cleanup.
- Idempotent/reconcilable mailer registration after a lost registration response.
- Full onboarding, task/profile forms and cross-host resource focus.
- Aggregate API/wire bounds and large-run context pagination.
- Inbound poison-record policy, recovery/requeue of terminal outgoing records, and grant/node lifecycle operations.
- Optional project/cross-run memory and Markdown projection.
- Stronger model-facing validation beyond the isolated continuous-service workflow and protocol fixtures.

No push, public release, host changes or deployment has occurred. Existing user sessions and shared runtime servers were not modified.
