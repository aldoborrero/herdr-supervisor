# Validation and completion status

Reviewed 2026-10-05. This document separates implemented/tested behavior from the full delivery contract in [IMPLEMENTATION.md](../IMPLEMENTATION.md). Passing the current suite does not establish completion of the supervisor.

## Evidence collected

The local implementation suite currently passes **398 unit tests and 6 CLI tests**, including the inherited Projects suite. The controller tests use private temporary state; real Git, SQLite, sockets, native child processes, a pseudoterminal and isolated NATS brokers are exercised. Runtime tests replace the model executables and Herdr/mailer boundaries with controlled fixtures.

| Requirement | Evidence | Limit |
| --- | --- | --- |
| Atomic capacity, DAG/revisions and operation replay | `src/control/tests.rs`: concurrent dispatch, dependency cycles, retired supervisors and transaction rollback; candidate tests schedule a reviewer while the author occupies the worker slot | Broader replanning/abandonment of an uncertain verifier pending |
| Unknown remote branch, inventory corruption and project polling | `src/coordinator_review.rs` plus inherited suite | Managed projection now tested separately; automated onboarding still pending |
| Signed origin and route authorization | Identity tampering/expiry tests, controller receive tests, real NATS ACL test | Explicit node rotation/revocation commands and production ACL template pending |
| Durable offline transport | Real JetStream restart/offline test; ACK follows SQLite acceptance | Retry/backoff, poison-record policy and expiry handling need further work |
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
| Native coordinator focus | `managed_open_focuses_only_the_exact_observed_native_coordinator` | Exact UUID/runtime/terminal checked against simulated Herdr; no model launch |
| Curated memory | `memory_is_curated_versioned_scoped_and_budgeted` | Run scope; live change delivery and project memory pending |
| CLI bootstrap usability | `managed_control_help_and_bootstrap_are_usable_without_herdr` | Does not deploy services |
| Combined candidate, independent review and owned Git CAS | `combined_candidate_review_and_checks_gate_owned_branch_for_sha1_and_sha256` uses real Git repositories, a divergent accepted base and an untouched dirty user checkout | Model review content simulated; code contracts only |
| Conflicts and check evidence | `conflicts_are_explicit_and_do_not_change_either_checkout`, `failing_or_mutating_checks_cannot_be_replaced_by_an_author_claim`, `started_check_is_not_rerun_after_unknown_execution_outcome` | Repair/replanning needs explicit lifecycle support |
| Git effect followed by SQLite failure | `git_cas_after_sql_failure_is_reconciled_once_without_reapplying` injects SQL failure after real Git CAS, reopens the store and verifies one final receipt | Owned refs must remain reserved from independent Git writers |
| Ref protection | `checked_out_integration_branch_and_changed_base_are_never_overwritten` | Git clients outside the controller do not participate in its lock |
| Remote result import and ancestry | `remote_result_requires_configured_node_source_and_exact_ancestry` clones a separate repository and imports only from the configured node source | Local Git transport; SSH credentials/connectivity are deployment configuration |
| Schema migration | `migration_preserves_v2_authority_credentials_and_operation_replays` retains live v2 rows, signed outbox and operation replay across migration | No downgrade supported |
| Controller service | `continuous_controller_verifies_without_blocking_api_and_applies_owned_ref` runs the service with real NATS and Git; a gated check keeps running while a Unix API snapshot succeeds | One-cycle service mode; no authenticated models |
| Reviewer transport | `assigned_reviewer_uses_signed_node_transport_and_receives_controller_receipt` uses a restricted NATS node account and the worker boundary | The review decision is simulated; broker ACK cannot approve it |
| Native exit and durable capacity release | `normal_exit_receipt_survives_restart_and_loss_between_event_and_job_update` waits for a real child, restarts node state and injects failure after event enqueue | Native-process scope, not a detached-descendant or remote-provider audit |
| Cancellation and deadlines | `cancellation_stops_owned_child_and_fences_workers_before_releasing_capacity`, `deadline_stops_native_process_without_a_live_controller_and_controller_expires_run` | Real harmless children, including TERM refusal and KILL escalation; no model sessions |
| Missing receipt / stale PID | `missing_exit_receipt_never_signals_a_saved_pid_or_frees_capacity` points the stored PID at a separate live test process | Unknown wrapper outcomes remain occupied; no automatic orphan recovery |
| Prelaunch cancellation and wrapper failure | `cancellation_before_launch_is_provable_and_late_execute_stays_fenced`, `spawn_failure_and_wrapper_signal_have_durable_terminal_receipts` | No automatic pane or worktree deletion |
| Interactive terminal | `interactive_native_child_gets_foreground_terminal_and_wrapper_restores_it` creates a real PTY and sends input to the foreground native child | Linux host evidence; no live Herdr/model session |
| Node schema migration | `node_v2_migration_preserves_pending_events_and_does_not_invent_process_receipts` | Old launcher instances remain uncertain without new exit receipts |
| Repair and reviewer replacement | `stopped_reviewer_can_be_replaced_and_rejected_task_retries_with_new_evidence` | Does not abandon in-flight check or Git intents |
| Run completion | Combined-candidate test refuses completion while processes remain, then completes, replays and releases the repository binding | Cancelled-run binding reconciliation remains pending |

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

## Commands

```bash
NATS_SERVER=/absolute/path/to/nats-server cargo test --locked --offline
nix develop --command cargo clippy --locked --offline --all-targets -- -D warnings
nix fmt
nix build .#herdr-supervisor --cores 2 --max-jobs 1 --print-build-logs
nix flake check --cores 2 --max-jobs 1 --print-build-logs
```

The Nix build of the controller/transport foundation passed on `x86_64-linux`. Package and flake checks are required again after executor changes; the local implementation commit records the completed gates for its snapshot. Some build attempts exhausted the shared filesystem, including one after the local suite passed. Only generated task compilation caches were removed manually; unrelated temporary directories and system-wide garbage collection were not touched.

Mailer protocol-2 support is an independent local commit **`7a12d5a`** on `feat/supervisor-delivery`, based on `939cff1`. `nix flake check` passes for that commit on `x86_64-linux`; the package runs Go tests, race, vet, real NATS tests and native bridge SDK tests offline. Supervisor sends require its operation-ID and recipient-precondition capabilities.

## Still required

- Artifact verification without a synthetic code commit; broader replanning and abandonment of an uncertain verifier.
- Cancelled-run repository binding reconciliation and conservative verification checkout cleanup.
- Recovery after loss of the native wrapper; detached-descendant audit and conservative resource cleanup.
- Idempotent/reconcilable mailer registration after a lost registration response.
- Full onboarding, task/profile forms, cross-host resource focus and asynchronous managed view refresh.
- Aggregate API/wire bounds and large-run context pagination.
- NATS backoff/expiry handling, node lifecycle operations and deployment ACL examples.
- Live memory change consumption and optional Markdown projection.
- Opt-in NixOS/Home Manager modules and their evaluation tests.
- A complete isolated workflow through the continuous node service, plus stronger model-facing validation.

No push, public release, host changes or deployment has occurred. Existing user sessions and shared runtime servers were not modified.
