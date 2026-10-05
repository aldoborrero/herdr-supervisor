# Validation and completion status

Reviewed 2026-10-05. This document separates implemented/tested behavior from the full delivery contract in [IMPLEMENTATION.md](../IMPLEMENTATION.md). Passing the current suite does not establish completion of the supervisor.

## Evidence collected

**Last completed full Nix gate:** commit `d7f2aa33df60d41a92258f0d520edbc38e4495ad` passes package build and `nix flake check` on x86_64-linux, with 497 unit and 10 CLI tests offline. It includes explicit prelaunch registration recovery and the separate recovery ledger. The Codex live-ownership extension still requires its package gate. This does not establish other platforms or completion of the remaining feature scope.

The local implementation suite currently passes **500 unit tests and 10 CLI tests** (local suite with four test threads), including the inherited Projects suite. The controller tests use private temporary state; real Git, SQLite, sockets, native child processes, a pseudoterminal and isolated NATS brokers are exercised. Runtime tests replace the model executables and Herdr/mailer boundaries with controlled fixtures.

| Requirement | Evidence | Limit |
| --- | --- | --- |
| Atomic capacity, DAG/revisions and operation replay | `src/control/tests.rs`: concurrent dispatch, dependency cycles, retired supervisors and transaction rollback; candidate tests schedule a reviewer while the author occupies the worker slot | Broader replanning/abandonment of an uncertain verifier pending |
| Unknown remote branch, inventory corruption and project polling | `src/coordinator_review.rs` plus inherited suite | Managed projection now tested separately; automated onboarding still pending |
| Signed origin and route authorization | Identity tampering/expiry tests, controller receive tests, real NATS ACL tests | Explicit node rotation/revocation commands remain pending |
| Reply inbox confidentiality and broker management | `real_broker_generated_acl_isolates_reply_inboxes_and_runtime_management` requires actual NATS permission errors for foreign reply subscriptions, consumer reads, stream mutations, message reads and ACKs; confirms legitimate deliveries/ACKs/reconnects | Generated runtime policy must be applied to the deployed broker; administrators remain trusted |
| Broker policy usability and reserved routes | `broker_routes_reject_reserved_names_and_wildcards_before_enrollment_or_grant`; continuous CLI workflow invokes `broker-policy` and installs its permissions on separate controller/node users | Example in `docs/nats-acl.md`; existing host/broker configuration is not changed |
| Durable offline transport | Real JetStream restart/offline test; ACK follows SQLite acceptance | Recovery of terminal outgoing records remains pending |
| Poisoned consumer windows | `poisoned_broker_windows_cannot_starve_valid_commands_and_events`: 65 malformed records per endpoint followed by a valid command/event, real restricted NATS clients, zero broker pending deliveries | TERM follows local persistence, not application acceptance; bounded storage can still exhaust |
| Failed local quarantine write | `failed_local_hold_never_terminates_broker_delivery` injects SQLite failure on both endpoints and observes one outstanding broker delivery each | No TERM, command or applied inbox row is written |
| Deferred prerequisites, restart and lost local receipt | `missing_assignment_retries_after_restart_and_lost_application_receipt`; `deferred_controller_application_survives_restart_and_creates_one_receipt` | Real broker/SQLite; no models; retry runs full original authorization |
| Incoming integrity, limits and maintenance | `reception_checks_integrity_time_scope_and_never_renews_or_leaks_records`; `reception_limits_fail_closed_and_oversize_documents_retain_no_body`; continuous CLI workflow uses status/prune | 4,096 rows, 64 MiB raw bodies, no automatic terminal eviction; pending entries survive pruning |
| Deferred execution fencing | `frozen_reception_bytes_and_cancellation_fence_deferred_execution` | Changed stored bytes rejected; cancellation tombstone prevents delayed launch |
| Controller/node schema 6 | `schema_five_upgrade_preserves_existing_jobs_and_frozen_outbox`; earlier migration tests updated to include the new table | Existing event identities, frozen outbox and jobs preserved |
| Durable retry and lost SQL publication receipt | `broker_acceptance_with_lost_sql_receipt_retries_identical_bytes_after_restart` injects failure after real broker storage, reopens SQLite and checks unchanged wire/ID, broker dedup and controller application | Network timeout alone never establishes acceptance |
| Expired, malformed and premature outgoing records | `expired_malformed_and_future_outgoing_records_do_not_block_commands_or_receipts` keeps expired/invalid rows, defers a future timestamp and preserves active process capacity | Outgoing terminal states do not establish task outcomes |
| Fairness across denied routes | `denied_outgoing_route_does_not_starve_incoming_events_or_other_nodes` receives a worker proposal and delivers its receipt while another destination is denied by real NATS ACLs | Incoming consumer/service outages remain separate failures |
| Bounded retry schedule and private inspection | `retry_schedule_survives_restart_is_bounded_and_operator_projection_omits_credentials`; continuous CLI workflow invokes `delivery-status` | At most 100 recent metadata rows, with full state counts; no terminal requeue command |
| Retry migration and native binding lookup | `node_v4_retry_migration_preserves_wire_jobs_memory_and_idempotent_operations`; extended three-runtime bootstrap test retains a malformed event row | Node schema 5; no native model calls or grant renewal |
| Artifact content before immutable result | `artifact_result_requires_verified_bytes_and_seals_exact_attempt_contract_and_manifest`, `artifact_hash_mismatch_and_empty_files_have_no_false_completion` | Stored hashes alone are insufficient; separate artifact pipeline tests below cover checks and independent acceptance |
| Artifact replay, corruption and rollback | `artifact_chunks_are_immutable_replayable_and_finalization_rolls_back_with_journal`, `concurrent_artifact_chunks_commit_one_complete_content_and_manifest` | Journal contains metadata/digests; private signed inboxes still retain wire bytes |
| Artifact authorization and bounded reservation | `artifact_scope_and_epoch_checks_apply_at_node_and_authority`, `artifact_shape_paths_and_reserved_capacity_are_bounded` | Author/attempt/epoch scope; logical 8 MiB file, 32-file/32 MiB attempt limits |
| Signed artifact transport and prerequisite ordering | `real_nats_artifact_chunks_recover_result_before_content_across_restart` | Real restricted NATS clients, no model; each wire fits the existing 300,000-byte frame |
| Upload CLI, IPC contention and status | `artifact_file_upload_retries_busy_ipc_resumes_exact_events_and_rejects_symlinks`, `artifact_status_remains_readable_during_execution_fence_and_rejects_retired_authority`; continuous CLI service workflow uploads binary content twice and inspects verification | No independent artifact review or completion claim |
| Cancellation before bulk receipts | `artifact_receipt_backlog_cannot_delay_due_cancellation` | Priority applies to due controller cancellation commands, not a guarantee during broker/SQL outages |
| Artifact migration | `controller_artifact_schema_upgrade_preserves_prior_authority_and_frozen_records`; updated earlier migration fixtures | Controller schema 7; node remains 6 |
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
| Lost registration does not rotate a receiver | `coordinator_lost_registration_stays_uncertain_without_rotating_mailer_session` | Simulated mailer; explicit prelaunch recovery is covered below, post-launch recovery remains pending |
| Native binding, process ownership and generation retirement | `three_coordinator_runtimes_bind_exact_native_sessions_bootstrap_and_stop_on_retirement` | Real harmless child processes and Unix IPC, mocked Herdr/mailer; no authenticated models |
| Late wrapper is fenced after cancellation | `fenced_unclaimed_coordinator_cannot_start_late` | No process spawned for the retired generation |
| Schema migration preserves prior authority | `schema_four_migration_preserves_active_coordinator_routes_and_memory` | SQLite schema 4 to 5; routes, notices and approved memory retained |
| Service restart reconciles launch intents | `controller_service_pumps_coordinator_intents_and_reconciles_after_restart` | Real NATS and stub Herdr executable; two service cycles, one create attempt |
| Projects submits once and displays pending state | `managed_open_reserves_once_and_displays_pending_launch_without_legacy_effects` | Real controller API; owner launch configuration required |

## Nix service modules

`checks.<system>.service-modules` evaluates the actual exported NixOS and Home Manager modules with the pinned inputs. Linux checks cover disabled imports, the publishing package default, explicit non-root ownership, controller/node commands, multiple nodes, distinct normalized state, absent runtime configuration files, optional coordinator notifications, dependencies, private masks, restart limits, SIGINT/drain behavior and literal systemd argument escaping. It also reads the rendered unit files from both module systems. Darwin checks disabled imports and rejection of enabled Linux services. These are evaluation/rendering checks; no existing host or user manager is activated. See [services](services.md).

The service-module checkpoint passes 36 Linux assertions and rendered-unit checks, plus evaluation for aarch64-linux and the Darwin rejection case. The cancellation-recovery checkpoint (`1a5afe9`) passes full `nix flake check` on x86_64-linux, including package compilation and its 410 unit/7 CLI tests, the service-module checks and the formatter check. This closes the earlier disk-full validation backlog for the modules, continuous workflow, live memory and cancellation recovery. The asynchronous-view checkpoint adds five regressions listed above and uses the same full package gates. This does not establish builds on the other declared platforms.

The same source also passes `cargo test --locked --offline` and strict all-target Clippy using a task-local RAM-backed Cargo cache. Earlier `nix flake check --no-build` results established evaluation only; they have now been supplemented by the full package gate. The successful Nix build uses the normal daemon build directory. No shared permissions or daemon settings were changed.

The asynchronous-view commit (`62baee8`) passes the 415 unit/7 CLI tests, strict all-target Clippy and `nix fmt` locally. Its full Nix build compiled the application but failed while compiling the check binaries with `rustc-LLVM ERROR: IO failure on output stream: No space left on device`. The chained flake check did not run. Full validation was pending at that checkpoint; the later `5afb1d9` package now covers it. `result` now points to that artifact-content package.

The inbox-ACL commit (`0cd387b`) also passed its 417 unit/7 CLI tests, strict Clippy and formatting locally. Retrying its Nix build with one compilation job reached the check phase but again exhausted the shared filesystem. The subsequent outgoing-retry source passes 422 unit/7 CLI tests and strict Clippy using the RAM-backed Cargo cache. Full Nix validation was pending then and is now supplied by `5afb1d9`, which includes those changes.

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

- Broader replanning and abandonment of an uncertain verifier.
- Recovery/abandonment of uncertain cancelled-run effects and conservative verification checkout cleanup.
- Recovery after loss of the native wrapper; detached-descendant audit and conservative resource cleanup.
- Reconciliation for legacy/lost-wrapper registration evidence. Explicit recovery covers prelaunch Claude/Pi and still-owned Codex before binding.
- Service onboarding and cross-host resource focus. Managed project creation now has a form and resumable CLI.
- Aggregate API/wire bounds and large-run context pagination.
- Recovery/requeue of terminal outgoing records and grant/node lifecycle operations.
- Optional project/cross-run memory and Markdown projection.
- Stronger model-facing validation beyond the isolated continuous-service workflow and protocol fixtures.

No push, public release, host changes or deployment has occurred. Existing user sessions and shared runtime servers were not modified.

The rejected-delivery change passes 430 unit tests (8.58 s), all 7 CLI tests (22.50 s after fixing the new fixture call) and strict all-target Clippy (21.32 s). The eight reception regressions include real restricted-broker delivery, saturation beyond the 64-message pending window, failed local persistence, missing prerequisites, restart, lost local application receipts, changed wire bytes, cancellation fencing, bounded retention/pruning and schema-5 migration. No live model, deployed broker or host configuration was changed. These were local Cargo results using the RAM cache; the later `5afb1d9` Nix package also validates the rejection-recovery changes.

For the same reception source, `nix fmt`, realization of `checks.x86_64-linux.service-modules` and `checks.x86_64-linux.pkgs-formatter-check`, and `nix flake check --no-build` pass. The last command evaluates derivations only; the later `5afb1d9` build/check supplies the full gate. The shared filesystem still had approximately 773 MiB available, so the previously failing full build was not restarted under equivalent conditions.

The artifact-content source passes 441 unit tests (7.29 s), 7 CLI tests (20.93 s) and strict all-target Clippy (9.16 s) using the RAM Cargo cache. The continuous workflow exposed executor-fence contention in the new read-only status path; snapshot reads now avoid that mutation fence and are covered by a regression. File upload retries only the known busy-fence error and preserves operation IDs. These results establish content transfer/sealing, not the pending artifact verifier/reviewer/acceptance stage. The full Nix package gate subsequently passed at `5afb1d9` after free space increased.

### Full package gate for artifact content

When approximately 2.1 GiB became available, the pinned local artifact-content commit was built with one compilation job and the normal daemon build directory. Application compilation took 5 min 13 s; the check phase took 5 min 41 s, including compilation and 441 unit tests (74.21 s) plus 7 CLI tests (20.52 s). Both the build and the chained full flake check exited successfully. The installed CLI help was also invoked successfully.

```bash
nix --option min-free 0 build \
  'git+file:///home/aldo/Dev/aldoborrero/herdr-supervisor?rev=5afb1d9a02b4649dec6ffa874d341c235d578ffd&shallow=1#herdr-supervisor' \
  --cores 1 --max-jobs 1 --print-build-logs
nix --option min-free 0 flake check \
  'git+file:///home/aldo/Dev/aldoborrero/herdr-supervisor?rev=5afb1d9a02b4649dec6ffa874d341c235d578ffd&shallow=1' \
  --cores 1 --max-jobs 1 --print-build-logs
```

The explicit pinned URL avoids this checkout's previously observed shallow-Git source lookup issue. Output: `/nix/store/v5vl9qs4rp3b36vs2z07wj9ch75c5g42-herdr-supervisor-0.1.0`, now referenced by `result`. Logs are `.direnv/artifacts-nix-build.log` and `.direnv/artifacts-nix-check.log` in the implementation worktree. The flake check includes the package, formatter and module checks; incompatible platforms were not built. No host/service activation, publication or shared daemon configuration change was performed. A documentation-only follow-up records this evidence; the tested implementation revision remains the pinned commit above.

## Artifact verification and independent acceptance

The new artifact pipeline has focused evidence separate from the earlier content-upload checkpoint:

| Invariant | Evidence | Limit |
| --- | --- | --- |
| Artifact identity, acceptance and process lifecycle | `artifact_pipeline_acceptance_completes_task_without_git_or_releasing_process_capacity` | Empty repository configuration and nonexistent source checkout; no Git binding is created; process exits remain separate |
| Checks cannot mutate declared inputs | `artifact_pipeline_successful_check_cannot_modify_its_inputs` | Executes a real shell check that changes a file and exits zero; the candidate still fails |
| Owned preparation and conservative recovery | `artifact_pipeline_materialization_recovers_only_owned_incomplete_inputs` | Refuses preexisting directories, recovers a lost ready receipt, rejects changed ready bytes and symlinks |
| Lost check outcome | `artifact_pipeline_lost_check_receipt_does_not_repeat_external_command` | Real append side effect occurs once; SQL fault leaves a started receipt and retry fails conservatively |
| Atomic acceptance and cancellation | `artifact_pipeline_acceptance_rolls_back_and_cancelled_retry_cannot_complete` | Injected journal failure rolls back task acceptance; cancellation retires the queued acceptance before process exit |
| Full content revalidation | `artifact_pipeline_acceptance_revalidates_source_and_materialized_bytes` | Source chunks or ready filesystem copies changed after checks prevent acceptance |
| Reviewer distribution and independent decision | `artifact_pipeline_cross_node_review_fetches_exact_content_over_restricted_nats` | Real restricted broker, two logical nodes, eighteen chunks, restart and exact manifest/role/epoch scope; simulated reviewer decision, not a live model |
| Node migration | `artifact_pipeline_node_schema_upgrade_preserves_jobs_and_pending_signed_events` | Schema 6 to 7 preserves existing jobs and frozen pending event bytes |
| Continuous artifact lifecycle | CLI `continuous_services_verify_artifacts_before_native_review_and_complete_without_git_integration` | Actual services and CLI upload, Git worktrees, native wrappers, IPC and NATS; fixture asserts files/hash/path at mailer bootstrap, criterion review and process exit before run completion; model/Herdr/mailer boundaries simulated |

The checkpoint passes `cargo test --locked --offline` (449 unit tests in 7.60 s and 8 CLI tests in 23.66 s), strict all-target Clippy (10.35 s after removing two redundant references), and `nix fmt`. The same artifact-pipeline commit `bec9f94` subsequently passed full package build and flake check; the coordinator-notice follow-up also passed its package gate as recorded below. No host, live session, shared broker or publication has been changed.

### Artifact review wakes the coordinator

A follow-up review found that the coordinator journal filter selected `submit_review` but omitted the new `submit_artifact_review` event. The cross-node artifact test now binds a native coordinator route and requires one pending notice for the exact reviewer after controller application. It failed against `bec9f94` (zero notices) and passes with the filter correction. All eight coordinator regressions and strict all-target Clippy also pass. Logs: `.direnv/artifact-notice-regression.log` and `.direnv/artifact-notice-fix.log`. This is durable notice creation; the existing notifier tests separately cover native delivery, lost responses and exact recipient fencing. The follow-up package gate subsequently passed at `a32074f`, as recorded below.

### Full package gate for the artifact pipeline

Commit `bec9f949376391bfbf64bd52dd1561d8ab72d49a` passes full `nix build` and `nix flake check` on x86_64-linux using the same explicit full-revision URL, `--cores 1 --max-jobs 1` and offline package tests. Build compilation took 5 min 42 s and the check phase 6 min 45 s, including all 449 unit tests (81.18 s) and 8 CLI tests (43.33 s). Formatter and module checks pass. The installed `control --help` smoke also passes. Output: `/nix/store/fcrrs7n2k5qslidfddnfz9h4n94c15an-herdr-supervisor-0.1.0`. Logs: `.direnv/artifact-pipeline-nix-build.log` and `.direnv/artifact-pipeline-nix-check.log`. This package predates the coordinator-notice fix described above; no validation claim is transferred to unbuilt changes.

### Full package gate for coordinator artifact notices

Commit `a32074ff1cc4e16edc99920e3df17376d54d10f8` passes full Nix build and flake check on x86_64-linux. Build compilation took 6 min 16 s; the check phase took 8 min 11 s, including 449 unit tests (86.71 s) and 8 CLI tests (42.71 s). Formatter and service-module checks pass; incompatible systems were omitted. Output: `/nix/store/1cj91g4sqqz8vnyl18p6z9x4mcbch9hz-herdr-supervisor-0.1.0`. Logs: `.direnv/artifact-notice-nix-build.log` and `.direnv/artifact-notice-nix-check.log`. This validates the notice fix and all prior artifact pipeline changes, not the subsequent project-creation work.

## Managed project creation and recovery

The new creation tests exercise actual controller handlers/SQLite and filesystem publication; the CLI regression starts the actual private controller API. No model is started.

| Behavior | Evidence |
| --- | --- |
| View appears only after its binding, one run/operation on replay | `managed_setup_is_visible_only_after_binding_and_replays_one_run_without_launching_models` |
| Lost controller response retains exact request identity | `managed_setup_lost_controller_response_reuses_frozen_operation_after_restart` |
| Lost publication receipt preserves operator edits | `managed_setup_lost_publication_receipt_recovers_without_overwriting_the_published_view` |
| Changed inputs/credentials cannot silently change the operation | `managed_setup_keeps_scope_and_credentials_frozen_after_a_lost_response` |
| Foreign directories and a racing empty destination survive | `managed_setup_refuses_foreign_destinations_and_atomic_publish_cannot_replace_even_empty_directories` |
| Wrong scope and removal after publication fail conservatively | `managed_setup_rejects_wrong_receipts_and_never_recreates_a_deleted_published_project` |
| Lease contention and invalid policy cannot issue a request | `managed_setup_private_intent_fences_contention_and_invalid_specs_make_no_project` |
| Declarative symlink allowed, mutable intent symlink refused | `managed_setup_allows_linked_declarative_config_but_refuses_linked_mutable_intent` |
| Form validation, literal argv, runtime selection, scrolling and error retention | Three `managed_setup_form` tests and two `managed_setup_popup` tests, including Ctrl-R replay with saved name |
| CLI creation/replay/resume/status and legacy fence | `managed_create_publishes_one_bound_view_through_real_controller_ipc_without_launching_agents` |

Service provisioning, later task/profile editing and cross-host focus remain outside this creation checkpoint. Setup receipts describe local evidence, not a current service health assessment. Interrupted staging cleanup is deliberately still explicit. The final local suite passes 462 unit tests (7.98 s), 9 CLI tests (22.01 s) and strict all-target Clippy (11.85 s), including name-only Ctrl-R recovery and literal JSON argv. Log: `.direnv/managed-setup-final.log`. Commit `3753538` subsequently passed its full package gate, recorded below.

### Plugin menu creation uses the managed form

A real pseudoterminal regression exposed a remaining legacy path: `pane new`, used by the plugin menu, still prompted for a name and immediately created/opened a legacy project. `plugin_new_pane_opens_managed_form_and_cancel_creates_no_project` failed against `3753538` with the old `Name:` prompt. The action now enters the same managed form as popup N, and the manifest labels it **Supervisor: new managed project**. The regression verifies controller/credential/commit/check fields and Ctrl-R help, exits through the real keyboard loop and confirms no project directory was created. This is actual terminal/CLI execution with isolated HOME; it does not use a Herdr server or a model. Logs: `.direnv/managed-pane-before.log` and `.direnv/managed-pane-final.log`. The first test-harness revision was corrected to wait for the complete rendered form rather than a partial PTY read.

The menu correction passes the full local suite: 462 unit tests (10.17 s), 10 CLI tests (24.60 s), strict all-target Clippy (18.71 s), and `nix fmt`. The Nix build/check for `3753538` subsequently passed and predates this menu correction; its result is not transferred to the follow-up source.

### Full package gate for managed project creation

Commit `375353812633ff0314e6c924f784d22dd961a934` passes full Nix build and flake check on x86_64-linux. Build compilation took 7 min 14 s, and the check phase took 4 min 51 s, including 462 unit tests (80.53 s) and 9 CLI tests (41.93 s). Formatter and service-module checks pass; incompatible systems were omitted. Output: `/nix/store/fq0bwah8v32wfki9vql5ic9cp510b2hk-herdr-supervisor-0.1.0`. Installed `managed --help` confirms create/resume-setup/setup-status. Logs: `.direnv/managed-setup-nix-build.log` and `.direnv/managed-setup-nix-check.log`. This evidence predates the plugin-menu correction and task form.

## Task creation from the managed popup

| Behavior | Evidence |
| --- | --- |
| Allowed checks, exact criteria/instructions and same-run dependencies | `managed_task_form_validates_policy_dependencies_and_preserves_literal_contract` |
| Lost response retries exactly the captured operation and revision | `managed_task_form_freezes_uncertain_request_and_never_rebases_a_retry` |
| Foreign context and inactive runs fail; code/artifact selection | `managed_task_form_rejects_foreign_scope_and_paused_runs_and_supports_artifacts` |
| Contract above the OS single-argument limit goes over stdin | `managed_task_form_large_contract_travels_on_stdin_with_bounded_argv`; existing real-IPC creation CLI test now submits/replays 65,536 backslashes as instructions and checks one resulting task |
| Failed submissions stay frozen; background refresh cannot retarget; stale view cannot open | `managed_task_form_popup_refuses_stale_rows_and_freezes_failed_submission` |
| Actual authority accepts exact replay once and rejects concurrent stale creation | `managed_task_form_applies_one_exact_contract_with_dependencies_and_rejects_stale_creation`, with actual private API/SQLite, persisted dependency and zero attempts |

These tests establish task entry and exact submission, not dispatch, task/profile editing or live model processing. Commit `102e590` subsequently passed its full package gate, recorded below.

The task-form source passes the full local suite: 468 unit tests (7.69 s), 10 CLI tests (21.33 s) and strict all-target Clippy (10.57 s). Log: `.direnv/managed-task-form-full.log`. This includes the earlier real-pseudoterminal plugin-menu regression.

## Planned task amendment and coordination

| Invariant | Evidence |
| --- | --- |
| Contract/hash/dependency replacement, historical audit and restart replay | `amend_task_replaces_contract_edges_and_hash_once_preserving_prior_audit` |
| Cycle/self/foreign/missing edges, unauthorized check and journal failure preserve old state | `amend_task_cycle_foreign_edges_invalid_checks_and_journal_failure_roll_back_everything` |
| Worker/cross-run/stale/live-task/paused-run rejection; old receipt cannot restore old contract | `amend_task_fences_worker_foreign_scope_live_tasks_and_stale_revisions` |
| Concurrent edit versus dispatch selects one exact contract | `amend_task_and_dispatch_race_keep_one_contract_and_exact_worker_order` runs eight actual SQLite races and inspects the persisted execute order |
| Form prefill, whitespace, dependency list and task revision | `amend_task_form_preserves_fields_and_uses_task_revision_and_exact_dependencies`; `amend_task_form_refuses_live_unknown_self_dependent_or_incomplete_context` |
| Stale-view guard, retained editor and exact stdin retry | `amend_task_popup_opens_existing_contract_and_refuses_edit_from_stale_view` |
| Native notice intent for successful planning decisions | `amended_task_generates_one_coordinator_notice_and_replay_does_not_repeat_it`; `operator_created_task_and_dependency_changes_wake_coordinator_once_each` |
| Actual CLI/private IPC amendment and replay | The managed creation CLI regression now edits its task, verifies a changed hash/revision and replays the older creation receipt without restoring old state |

The new mutation uses the existing scoped controller API and MCP facade. Tests exercise SQLite transactions, real private IPC and the actual CLI; no live model or shared service is involved. New notice tests establish journal-to-notice persistence; existing notifier tests cover native delivery separately. The coordinator instruction update describes artifact verification and acceptance already covered by the artifact pipeline tests. It does not establish model compliance. The full local suite passes 477 unit tests (7.96 s), 10 CLI tests (22.66 s) and strict all-target Clippy (12.69 s). Log: `.direnv/task-amend-full.log`. The full package gate subsequently passed at `f05d59c`, as recorded below.

### Full package gate for task creation and plugin menu

Commit `102e590662e6bfda91ce3660f7de4f9a40d30905` passes full Nix build and flake check on x86_64-linux. Compilation took 5 min 50 s and check phase 4 min 58 s, including 468 unit tests (79.99 s) and 10 CLI tests (44.92 s). Formatter/service-module checks pass; incompatible platforms were omitted. Output: `/nix/store/3bkkf5541vy7vjq4sgf2jp7067nslcbj-herdr-supervisor-0.1.0`. Logs: `.direnv/managed-task-form-nix-build.log` and `.direnv/managed-task-form-nix-check.log`. The real-pseudoterminal menu test and large stdin contract run inside the package gate. This predates planned-task amendment and its new notice events.

After a final help-text adjustment excluding the task itself from predecessor choices, the two amendment-form regressions and strict all-target Clippy pass again (`.direnv/task-amend-final.log`). `nix fmt` passes for the amendment source. No host/service activation or publication occurred.

## Planned-task amendment package gate

Commit `f05d59c48dc6613279c91b1e52c87d2f1477bc73` passes full Nix build and flake check on x86_64-linux. Compilation took 6 min 1 s and check phase 5 min 6 s, including 477 unit tests (80.34 s) and 10 CLI tests (43.44 s). Formatter/service-module checks pass; incompatible platforms were omitted. Output: `/nix/store/fgjnhdllzr45fsphy03f85im65kr07k4-herdr-supervisor-0.1.0`. Logs: `.direnv/task-amend-nix-build.log` and `.direnv/task-amend-nix-check.log`.

## Owner-managed profiles

| Requirement | Evidence |
| --- | --- |
| Owner authority, exact revision, replay, explicit deletion and argument restrictions | `run_profiles_are_owner_scoped_revisioned_and_cannot_override_permissions_or_remove_last_profile` |
| Existing assignments survive edits/removal; future reservations obey new limits | `run_profiles_edit_and_removal_preserve_reserved_assignments_and_future_capacity_limits` |
| Atomic profile snapshots and journal failures | `run_profiles_snapshot_and_profile_update_roll_back_with_failed_outbox_or_journal` |
| Schema-7 migration preserves original signed bytes and assigned profile | `run_profiles_v7_migration_preserves_signed_bytes_and_original_profile_after_policy_change` |
| Conflicting/missing legacy evidence does not invent authority | `run_profiles_migration_rejects_conflicting_frozen_intent_without_partial_upgrade`, `run_profiles_missing_legacy_order_stays_unknown_instead_of_borrowing_current_policy` |
| Original checks/review remain valid after profile edit; other policy changes fail | `run_profiles_updates_preserve_migrated_check_receipts_and_inflight_reviewer_but_not_other_policy_changes` |
| Coordinator launch retains assigned settings and receives profile-change notice once | `run_profiles_changes_preserve_coordinator_launch_snapshot`, `run_profiles_owner_update_is_visible_and_notifies_coordinator_once` |
| Owner-only form, fixed name, literal argv, validation and frozen request | `run_profiles_form_is_owner_only_and_preserves_name_argv_and_run_revision`, `run_profiles_form_validates_permission_flags_name_and_limits_before_freezing` |
| Popup owner/stale fencing, failure replay and captured removal revision | `run_profiles_popup_fences_owner_edits_and_freezes_failures_and_removal_revision` |
| Actual CLI mutation/replay through private IPC | Extended `managed_create_publishes_one_bound_view_through_real_controller_ipc_without_launching_agents` |

The full local suite passes 489 unit tests (8.07 s), 10 CLI tests (26.54 s) and strict all-target Clippy (12.33 s). Log: `.direnv/run-profiles-full.log`. Tests use real SQLite/Git/IPC and controlled runtime boundaries, not live model sessions. Controller schema is 8; node schema remains 7. Commit `010cbd7` subsequently passed its full Nix package gate, as recorded below. Original policy hashes and receipts are retained; compatibility excludes only the profiles map, not checks, review requirements or budgets.

## Prelaunch registration recovery

| Requirement | Evidence |
| --- | --- |
| Explicit coordinator authorization followed by one launch, no automatic rotation | `registration_recovery_coordinator_retries_only_after_explicit_prelaunch_authorization` |
| Coordinator files/symlinks, process evidence, generation/lease fencing and rollback | `registration_recovery_coordinator_fences_files_processes_generation_and_rolls_back_journal_failure` |
| Node restart and exact operation replay without assignment/native UUID changes | `registration_recovery_node_is_durable_idempotent_and_preserves_original_assignment` |
| Node launch/process/file/epoch/cancellation fences | `registration_recovery_node_rejects_process_files_stale_epoch_and_launch_markers` |
| Atomic checkpoint failure and bounded retained history | `registration_recovery_node_rolls_back_failed_checkpoint_and_bounds_history` |
| Schema migration preserves prior checkpoint bytes | `registration_recovery_schema_upgrade_preserves_prior_receipts_and_rejects_older_recovery` |
| Original signed command expiry, owner route, node epoch and content integrity | `registration_recovery_node_requires_unexpired_original_owner_route_and_content` |
| Lost preregistration response followed by real wrapper/bootstrap with new mailer epoch | `registration_recovery_workers_resume_native_launch_after_lost_registration_response` |
| Actual local CLI and replay with no secret output | Extended `managed_create_publishes_one_bound_view_through_real_controller_ipc_without_launching_agents` |

These tests use private SQLite/IPC and harmless native subprocesses. Herdr/mailer/model behavior is simulated; they do not prove live model consumption. At this checkpoint Codex had only ordinary bootstrap coverage; the subsequent live-ownership extension below adds its pre-binding recovery. The CLI records authorization without performing a remote registration, and its replay is historical rather than a current health claim. Controller schema 9 and node schema 8 add a separate SQLite recovery ledger and fence older service binaries. The full local suite passes 497 unit tests (8.12 s), 10 CLI tests (23.05 s) and strict all-target Clippy (12.62 s). The additional receipt-write fault checks that authorization rolls back with its missing receipt. Commit `d7f2aa3` subsequently passed its full package gate. Logs: `.direnv/registration-recovery-focused.log` and `.direnv/registration-recovery-full.log`.

## Owner-profile package gate

Commit `010cbd7b47fc31e1ef62646a8ad243491ddd8cd8` passes full Nix build and flake check on x86_64-linux. Compilation took 7 min 12 s and check phase 6 min 46 s, including 489 unit tests (95.79 s) and 10 CLI tests (48.81 s). Formatter/service-module checks pass; incompatible platforms were omitted. Output: `/nix/store/7ahyvkcwyaaiznk0agb8phv2l89lplgs-herdr-supervisor-0.1.0`. Logs: `.direnv/run-profiles-nix-build.log` and `.direnv/run-profiles-nix-check.log`. This package predates the registration-recovery change and its schemas.

## Prelaunch recovery package gate

Commit `d7f2aa33df60d41a92258f0d520edbc38e4495ad` passes full Nix build and flake check on x86_64-linux. Compilation took 6 min 35 s and check phase 5 min 22 s, including 497 unit tests (94.43 s) and 10 CLI tests (43.77 s). Formatter/service-module checks pass; incompatible platforms were omitted. Output: `/nix/store/p4i4vvw90aaj4qxfzm5rkrzbjbknnvk6-herdr-supervisor-0.1.0`. Logs: `.direnv/registration-recovery-nix-build.log` and `.direnv/registration-recovery-nix-check.log`. This predates Codex recovery with positive live-wrapper evidence.

## Codex recovery and wrapper ownership

| Requirement | Evidence |
| --- | --- |
| Recover one existing Codex without another launch; bind/bootstrap use the new receiver epoch | Extended `registration_recovery_workers_resume_native_launch_after_lost_registration_response`, retaining all three runtime routes |
| Exact live coordinator, mismatched UUID/terminal/PID, missing legacy ownership evidence, replay and no recovery after binding | `codex_registration_recovery_checks_live_owner_native_identity_and_fresh_snapshot` |
| Process exit can be written during observation and prevents stale authorization; unrelated writes permit recovery | Same coordinator test mutates SQLite from the Herdr observation fixture |
| Positive lock holder, private regular file, no symlink or path traversal | `native_ownership_lease_requires_live_holder_and_rejects_missing_symlink_or_public_file` |
| Wrapper crash releases ownership despite a living native child; capacity remains occupied | `native_owner_crash_releases_lease_without_claiming_its_child_exited` |

The wrappers and harmless child processes are real; Herdr/mailer/model boundaries remain simulated. The first fully parallel local run passed 498 of 500 unit tests but failed two existing NATS cases with connection refusal/request timeout; log `.direnv/codex-registration-first-failure.log` retains the evidence. Both cases pass independently, and the complete suite passes with four test threads: 500 unit tests (31.03 s) and 10 CLI tests (22.46 s). After the Clippy-requested late-initialization cleanup, nine recovery regressions and both ownership regressions pass again, with strict all-target Clippy (10.82 s). No assertion is skipped or weakened. The package gate remains pending. Logs: `.direnv/codex-registration-focused.log`, `.direnv/codex-registration-guards.log`, `.direnv/codex-registration-full.log` and `.direnv/codex-registration-final.log`. Ownership is positive evidence only: it never establishes exit when absent, and legacy process receipts are not upgraded by guessing.
