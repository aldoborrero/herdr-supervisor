# Validation and completion status

Reviewed 2026-10-05. This document separates implemented/tested behavior from the full delivery contract in [IMPLEMENTATION.md](../IMPLEMENTATION.md). Passing the current suite does not establish completion of the supervisor.

## Evidence collected

### Explicit local worker-checkout retirement

`cleanup-retire` records a local operator's exact attempt/epoch/HEAD decision and required process/retention attestations in node SQLite schema 10. It preserves the original branch plus a compare-and-swap retention ref before invoking Herdr without force. Successful reconciliation requires absence from the filesystem, Git worktree inventory and exact native resources, preserved refs and unchanged authority. An uncertain result reports `resources_deleted: null` and never repeats native removal. This is an explicit local maintenance action, not autonomous proof of descendant exit.

The first focused run passes ten matching unit tests, including the six initial retirement regressions, and strict Clippy (`.direnv/retirement-focused.log`). The first full run passes 600/604 unit tests and exposes four registration-recovery failures: its schema guard still expected node schema 9. That guard now accepts the new current schema 10; migration fixtures reconstruct their actual historical schemas before upgrade. `.direnv/retirement-full.log` retains the failure. The final source passes all 606 unit tests (58.65 s), 17 CLI tests (26.79 s) and strict all-target Clippy (29.26 s), including continuous code/artifact workflows, in `.direnv/retirement-final.log`. `nix fmt` passes, formatting only the Python fixture (`.direnv/retirement-fmt.log`); the package gate is pending.

| Evidence | Boundary established |
| --- | --- |
| `retirement_removes_only_confirmed_clean_checkout_and_retains_commits_and_replay` | Real temporary Git checkout removed through the Herdr fixture, result commits preserved in two refs, exact operation replay and conflicting decisions refused |
| `retirement_requires_operator_decision_positive_stop_and_exact_clean_ownership` | Missing stop, confirmations, changed epoch/HEAD/terminal, dirty and ignored files and busy executor prevent an intent or deletion |
| `retirement_lost_native_or_sql_reply_reconciles_once_without_repeating_removal` | Lost native response and failed final SQLite receipt recover without a second native effect |
| `retirement_uncertain_or_partial_removal_never_repeats_native_effect` | No-effect and checkout-only partial outcomes stay unresolved; separate fixture maintenance permits observation-only completion |
| `retirement_intent_failure_and_changed_retention_ref_preserve_checkout` | Failed SQLite intent prevents any removal; conflicting retention ref is neither overwritten nor ignored |
| `retirement_schema_upgrade_keeps_jobs_and_durable_removal_survives_restart` | Node schema-9 migration preserves job/plan bytes and the completed receipt survives restart |
| `retirement_operator_attestation_cannot_override_unresolved_or_foreign_native_exit` | Human confirmation cannot replace a required native exit or accept a foreign/mis-scoped receipt |
| `retirement_uncertain_reconciliation_fences_changed_authority_config_and_recreated_path` | A recreated path is retained, changed node epoch/repository mapping refuses reconciliation and uncertain deletion stays explicitly unknown |
| `retirement_cli_requires_explicit_bounded_decision_and_preserves_missing_state` | Actual scrubbed CLI requires confirmation, bounds the JSON request and never initializes missing node state |
| Extended continuous code workflow | Real wrapper stop, signed NATS controller-stop receipt, actual CLI retirement/replay of author and reviewer checkouts, and preserved Git result refs; Herdr/mailer/model boundaries remain fixtures |

No user checkout or live native session is removed by these tests. The operator's process attestation requires exclusive use of the checkout during retirement; it is not a kernel containment guarantee or an isolation boundary from other processes with the same UID. Verification/coordinator resources and unattended cleanup remain open.

### Independent verifier completion receipts

The verified source is committed locally as `f1d19112665c6e56df7741751f451ec5390e5177`. `nix fmt` also passed without changes (`.direnv/check-receipts-final-fmt.log`). Its pinned package build/check is running; do not infer success from an active tool handle.

The regression `check_receipt_recovers_finished_verifier_after_lost_sql_without_reexecuting` fails on `01d20d8`: the controller returns `checks_failed` after a completed check loses its SQLite receipt. `.direnv/check-receipts-before.log` records that failure, with a real Git checkout and an external execution counter. The implementation now atomically publishes an owner-signed completion file before the final SQL/audit transaction and recovers it without rerunning the check. Controller services reconcile these receipts before cancelled-run release; the local owner CLI performs the same bounded recovery. No schema migration is required; legacy starts without the receipt scheme remain unresolved.

The first focused implementation run recovered the code check but exposed the old artifact expectation (`checks_failed` despite a recoverable completion). That artifact test now expects `verified` while retaining its original assertion that the command ran only once. `.direnv/check-receipts-focused.log` records the transition. The signed implementation then passes eight focused unit tests (3.17 s), one CLI test (0.12 s) and strict all-target Clippy (34.81 s), in `.direnv/check-receipts-signed.log`. The initial full suite, including coherent-result tampering and legacy accounting, passes 597 unit tests (50.51 s), 16 CLI tests (23.09 s) and strict all-target Clippy (16.85 s), in `.direnv/check-receipts-full.log`. The final source recovers known failures before reobserving dirty inputs; all 598 unit tests (48.31 s), 16 CLI tests (22.87 s) and strict all-target Clippy (17.56 s) pass in `.direnv/check-receipts-final.log`. The Nix package gate for this checkpoint is pending.

| Evidence | Boundary established |
| --- | --- |
| `check_receipt_recovers_finished_verifier_after_lost_sql_without_reexecuting` | SQL-update loss, controller restart, exact recovered success and one actual command execution |
| `artifact_pipeline_lost_check_receipt_does_not_repeat_external_command` | Same durable recovery for artifact checks without inventing a Git candidate |
| `check_receipt_signed_provenance_and_outcome_cannot_be_substituted` | Owner/start/candidate/argv checks, signature verification and refusal of internally consistent substituted results/logs |
| `check_receipt_missing_partial_oversized_public_and_linked_files_never_become_results` | Missing, truncated, over-1-MiB, symlink and non-private files remain unresolved without repair or adoption |
| `check_receipt_failed_outcomes_and_dirty_checkout_evidence_remain_failed` | Nonzero exit and changed tracked files retain failure despite receipt recovery |
| `check_receipt_recovery_preserves_failed_artifact_inputs_without_repair_or_reexecution` | Dirty artifact inputs do not prevent applying their signed failure; verification becomes `checks_failed` while the changed files remain untouched |
| `check_receipt_recovery_is_leased_transactional_idempotent_and_fair` | Busy executor refusal, SQL/audit rollback, one journal decision, bounded rotation past 16 unavailable records and explicit legacy counts |
| `check_receipt_controller_service_recovers_before_releasing_a_cancelled_repository` | Real restricted NATS/controller cycle recovers completion before release, retains checkout and does not rerun the process |
| `check_receipt_reconciliation_cli_requires_owner_and_never_initializes_missing_state` | Actual scrubbed CLI, owner-token validation, no missing-state initialization, service start or model launch |
| `started_check_is_not_rerun_after_unknown_execution_outcome` | Missing completion evidence still prevents rerunning an uncertain original check |

This evidence covers completed processes with a published signed receipt. It does not recover a crash before that publication, a still-live verifier, detached descendants or cleanup. Owner-selected checks and private controller state retain the documented same-UID trust boundary; signatures do not isolate an owner key from a malicious process with the same filesystem access.

### Expired cancellation delivery

The run-scoped `renew_cancellation` decision retains an expired outbox intent, verifies its current attempt/epoch/node and original signed bytes when present, and queues a new cancellation delivery in the operation/audit transaction. Equal-epoch node acceptance requires identical prior cancellation evidence; a stopped job stays stopped. It does not renew execution authority, release capacity, extend the run deadline or prove native process exit. This uses existing schemas and ordinary signed cancellation commands; both endpoints need the updated replay behavior.

The backend checkpoint passes 590 unit tests (53.51 s), 15 CLI tests (25.39 s) and strict all-target Clippy (24.59 s), in `.direnv/cancellation-recovery-full.log`. The final source, including attempt-page metadata, Threads confirmation and coordinator instructions, passes 591 unit tests (56.59 s), 15 CLI tests (26.13 s) and strict all-target Clippy (29.83 s), in `.direnv/cancellation-recovery-ui-full.log`. `nix fmt` passes without changes (`.direnv/cancellation-recovery-fmt.log`). Commit `01d20d8cb932fda69ce599f2386251d88492621e` also passes Nix build and flake check on x86_64-linux with 591 unit tests (142.67 s) and 15 CLI tests (47.05 s) offline. Output: `/nix/store/8409p4x7h1a6kwbw8pw1hpjpf10nkkbc-herdr-supervisor-0.1.0`. Logs: `.direnv/cancellation-recovery-nix-build.log` and `.direnv/cancellation-recovery-nix-check.log`. These tests use isolated state, real IPC/NATS and simulated process evidence; none launches a native model or changes host services.

| Evidence | Boundary established |
| --- | --- |
| `cancellation_recovery_delivers_unseen_stop_and_replays_without_releasing_capacity` | Original bytes retained, new UUID/same cancellation, scoped current-delivery projection, operation replay after stop/restart and no capacity release until a stop event is applied |
| `cancellation_recovery_requires_scoped_current_intent_and_valid_original` | Node/foreign-run/stale-epoch refusal, unexpired refusal, original signature/recipient checks and recovery of an intent that expired before first signing |
| `cancellation_recovery_rolls_back_on_audit_failure_and_serializes_competing_renewals` | Failed audit rolls back outbox/operation changes; eight concurrent new operations produce one renewal |
| `cancellation_recovery_node_requires_identical_persisted_epoch_and_never_resurrects_stopped_job` | An equal epoch alone is insufficient; conflicting contents fail and a previously stopped job is unchanged |
| `cancellation_recovery_real_broker_handles_expiry_restart_and_preexisting_cancellation` | Restricted real broker, actual validity expiry, restarted node accepts a fresh delivery of a prior cancellation; only the later stop event releases capacity |
| `cancellation_recovery_private_api_and_mcp_share_scoped_operation_receipts` | Real controller socket, MCP decision/API replay and unauthorized node refusal without credentials in the response |
| `cancellation_recovery_popup_confirms_captured_delivery_and_disables_stale_or_current_stops` | Actual popup key/confirmation path keeps the captured command/epoch across refreshed rows; valid deliveries, stopped jobs and stale views have no retry action |

### Expired node-event recovery

`recover-event` renews transport authority around one immutable original signed event; it does not rewrite an old envelope, renew a worker capability or infer execution from broker storage. Controller recovery records point to an accepted inbox record, and node recovery records link each replacement to its original logical operation. New controller/node schemas are 11/9. Expired controller-origin commands, invalid original repair and native/Git uncertainty are not covered by this feature.

The full local run before the final regressions passes 582 unit tests (48.51 s), 14 CLI tests (22.16 s) and strict all-target Clippy (12.42 s), in `.direnv/event-recovery-full.log`. The final source passes all nine recovery unit tests (2.77 s), the new CLI regression (2.91 s) and strict all-target Clippy (12.88 s), in `.direnv/event-recovery-deadline.log`. `nix fmt` passes without changes (`.direnv/event-recovery-fmt.log`). Commit `013dc0f407f0c76cbaae7d409249ada809abaf5c` also passes the full x86_64-linux Nix build/flake gate with 584 unit tests (166.94 s) and 15 CLI tests (50.84 s) offline. Output: `/nix/store/m6536pgczvjydwlw708njkg189s67hb8-herdr-supervisor-0.1.0`. Logs: `.direnv/event-recovery-nix-build.log` and `.direnv/event-recovery-nix-check.log`. All runtime boundaries in this checkpoint use private fixtures; no native model or deployed service is involved.

| Evidence | Boundary established |
| --- | --- |
| `event_recovery_applies_expired_unseen_request_once_and_receipt_confirms_original_operation` | Original expired wire is retained; recovery is replayable, conflicting receipts fail and logical acceptance survives restart |
| `event_recovery_returns_prior_acceptance_after_worker_retirement_but_never_new_old_authority` | Positive accepted-history evidence permits receipt recovery after capability retirement and deadline expiry; an unseen old request gains no authority |
| `event_recovery_replays_a_prior_recovery_without_reapplying_and_rejects_origin_collisions` | Multiple deliveries preserve one decision; changed original content and nested recovery fail |
| `event_recovery_is_transactional_with_local_ledger_and_remote_receipt_failures` | Local ledger, controller application/outbox and node receipt failures roll back their whole transactions |
| `event_recovery_rejects_damaged_originals_foreign_signers_and_revoked_node_authority` | Original integrity, same signer/epoch and current enrollment remain mandatory |
| `event_recovery_wraps_a_maximum_original_record_without_shrinking_its_budget` | A complete 262,144-byte original fits the recovery-specific envelope budget; ordinary record limits stay unchanged |
| `event_recovery_real_nats_renews_after_lost_expired_receipt_and_node_restart` | Restricted real broker, actual grant/receipt expiry, renewed same-epoch grant and restarted node recover one accepted operation without reviving its retired worker |
| `event_recovery_concurrent_envelopes_commit_one_logical_decision` | Eight concurrent controller connections produce one decision and a receipt for each fresh delivery |
| `event_recovery_cannot_extend_a_run_deadline_but_can_confirm_a_late_stop` | No unseen work after deadline; positive late stop and previously earned receipts remain recoverable |
| `event_recovery_cli_renews_delivery_and_preserves_original_wire_without_services` | Actual isolated CLI queues/replays/inspects recovery without starting services, exposing tokens or initializing missing state |

### Owner-controlled node drain and revocation

This checkpoint adds controller schema 10, owner lifecycle mutations, status via CLI/API/MCP, drain-aware author/reviewer reservation and current-enrollment grant issuance. It does not revoke NATS credentials, terminate processes, invalidate a disconnected daemon's cached grant immediately or implement in-place key rotation. The original cleanup and native/remote acceptance gaps remain open.

The full local suite passes 575 unit tests (51.78 s), 14 CLI tests (25.62 s) and strict all-target Clippy (26.36 s), in `.direnv/node-lifecycle-full.log`. The earlier focused `node_` run passes 21 unit/2 CLI tests and Clippy, in `.direnv/node-lifecycle-focused.log`. `nix fmt` passes without changes (`.direnv/node-lifecycle-fmt.log`). Commit `49d6b1bda05a23f84fd7ed290890115d713a5b80` also passes Nix build and flake check on x86_64-linux with 575 unit tests (141.63 s) and 14 CLI tests (46.92 s) offline. Output: `/nix/store/dl31g6m49f3rvr39cjnkpp2hv770ng7c-herdr-supervisor-0.1.0`. Logs: `.direnv/node-lifecycle-nix-build.log` and `.direnv/node-lifecycle-nix-check.log`.

| Evidence | Boundary established |
| --- | --- |
| `node_drain_is_owner_revisioned_replayable_and_does_not_fence_existing_attempts` | Owner-only global decisions, run-facade refusal, exact revision, historical replay without reapplying drain, collision refusal, blocked future worker assignment and unchanged existing worker authority |
| `draining_node_refuses_new_reviewers_and_resumes_on_current_owner_revision` | Real Git candidate/check path cannot reserve a reviewer while draining; current owner resume permits the unchanged request |
| `node_revocation_requires_drain_and_confirmed_stop_and_preserves_signed_history` | Broker receipt alone cannot free authority; signed node/controller stop flow permits revocation, retires direct tokens and old signed replays, retains frozen bytes and refuses old/new-epoch grants or ID reuse |
| `node_revocation_rolls_back_authority_credentials_and_effects_with_failed_audit` | Failed journal write rolls back epoch, revision, credential retirement, outbox state and operation receipt; identical retry succeeds |
| `node_grants_match_current_enrollment_and_schema_upgrade_preserves_old_signed_bytes` | Exact active key/epoch required, schema-9 migration preserves existing signed bytes, and a partially inconsistent schema rolls back its attempted migration |
| `node_drain_serializes_with_dispatch_without_releasing_or_duplicating_capacity` | Eight concurrent drain/dispatch races preserve the single reservation if dispatch wins, or block dispatch; revocation never releases a surviving attempt |
| `node_revocation_real_broker_accepts_transport_but_controller_rejects_old_authority` | Restricted real NATS accounts, stop application during drain, broker acceptance after revocation, durable controller rejection rather than application ACK, and no extra accepted inbox record |
| `node_status_and_owner_drain_use_actual_api_and_mcp_authority` | Actual private controller socket, MCP mutation/read and denial of the supervisor's owner-only read |
| `node_lifecycle_cli_drains_revokes_replays_and_refuses_stale_grants` | Scrubbed CLI environment, isolated owner/node key generation, enrollment, grant, drain, revoke, replay and stale-grant refusal without starting services |

### Worker-checkout retention inspection

`cleanup-inspect` is a read-only prerequisite for cleanup, not an implementation of resource retirement. Its Git/SQLite tests exercise private linked worktrees and recorded node assignments with a simulated Herdr boundary. They cover dirty, untracked and ignored files; assume-unchanged/skip-worktree entries; changed branch/path/epoch/configuration; controller stop versus broker storage; native exit versus descendant evidence; incomplete or changed panes; authority changes during observation; restart; and disabled repository fsmonitor. No native model or deployed Herdr resource is used.

The full local suite passes 566 unit tests (40.57 s), 13 CLI tests (23.75 s) and strict all-target Clippy (12.57 s), in `.direnv/cleanup-inspection-full.log`. After strengthening the stop fixture to use the actual signed controller/node receive paths and adding a Git gitlink/foreign-stop regression, all six cleanup tests pass (1.46 s), followed by strict all-target Clippy (13.33 s), in `.direnv/cleanup-inspection-final-focused.log`. Commit `753dfd7fce03c6ba4720267e01184d39f67d5c5e` passes Nix build and flake check on x86_64-linux with 567 unit tests (153.41 s) and 13 CLI tests (46.24 s) offline. Output: `/nix/store/5qvxvc99wikwqm9g5mv2za3aid9cmb8n-herdr-supervisor-0.1.0`. Logs: `.direnv/cleanup-inspection-nix-build.log` and `.direnv/cleanup-inspection-nix-check.log`. The first focused run exposed a fixture error when clearing both Git index flags in one invocation; the fixture now clears each separately, preserving the dirty-file and hidden-entry assertions. Its earlier failing log is `.direnv/cleanup-inspection-focused.log`.

Commit `f6b87a2983ec32a3a4328cfac80cf57e1a45a125` passes x86_64-linux Nix build and flake check with 560 unit tests (124.25 s) and 12 CLI tests (43.86 s) offline. Output: `/nix/store/6m84lbfp5qbkyg89lwlzp4c2mx42zv8k-herdr-supervisor-0.1.0`. Logs: `.direnv/service-check-nix-build.log` and `.direnv/service-check-nix-check.log`. That gate covers `check-local`, not the subsequent cleanup inspection.

### Read-only local dependency diagnosis

`control check-local --bundle DIR` verifies a frozen bundle without initialization, repair, migration, registration, publication or model launch. Controller/node SQLite reads use read-only connections and read transactions; checks authenticate the pinned owner/node capabilities, original enrollment operation, current authority/epoch/capacity, grant and generated configuration manifest. A mismatch stops before external requests. NATS probes use only runtime accounts and consumer-info reads; Herdr uses an independent `supervisor-check` snapshot request; mailer uses only notifier capabilities/doctor. Errors from external endpoints are discarded in favor of static component actions. Git probes have a shared ten-second command budget. Authority is reread after external observations; this does not claim a globally atomic snapshot or native worker readiness.

Five focused tests pass (11.23 s), followed by strict all-target Clippy (22.84 s): `.direnv/service-check-focused.log`.

| Evidence | Boundary established |
| --- | --- |
| `check_local_missing_or_changed_bundle_is_not_repaired_and_sends_no_credentials` | No directories/databases created for absent state, no repair of a removed generated file, no external connection on integrity failure, foreign controller refusal and unchanged enrollment ledger |
| `check_local_observes_real_broker_git_and_private_protocols_without_decisions_or_admin_broker_use` | Restricted real NATS and Git, protocol-22 Herdr and protocol-2 mailer fixtures, absent broker admin credential, unchanged private bundle files/decision counts and no owner/node/admin token sent to a native probe |
| `check_local_reports_protocol_auth_repository_failures_without_external_error_text` | Wrong node broker account, old Herdr protocol, invalid repository and a malicious mailer error containing credentials produce failed component checks without leaking error text |
| `check_local_rechecks_retirement_after_successful_external_observations` | A node retired during snapshot handling invalidates the final result although its dependency probes succeeded |
| `check_local_rejects_oversized_mailer_frames_and_unhealthy_or_incomplete_doctor` | Bounded response, corrupt SQLite health and malformed sender session are refused |

The CLI regression expects structured JSON, nonzero status and no implicit initialization on missing state. Both continuous workflows now execute the real diagnostic CLI against their generated bundles and assert that no model/registration/launch or run revision changes occur. Existing watcher subscribe-before-snapshot assertions remain for event reconciliation; the independent preflight snapshot uses a distinct request ID. The final local suite passes 560 unit tests (47.24 s), 12 CLI tests (24.00 s) and strict all-target Clippy (21.68 s), in `.direnv/service-check-final.log`; the preceding full run also passed (`.direnv/service-check-full.log`). `nix fmt` formats the expanded Python fixture (`.direnv/service-check-fmt.log`). The package gate passes for `f6b87a2`, as recorded above. These tests use simulated native runtimes and cannot establish model authentication, native CLI compatibility, mailer admin authorization, repository write access or exclusive broker ACLs; the report names those untested boundaries.

### Resumable local service bundle

Commit `e1c24183d4624b8e7762dd0e8f8cb2dffeed1c43` passes x86_64-linux package build and flake check with 555 unit tests (120.05 s) and 11 CLI tests (45.61 s) offline. Output: `/nix/store/n0haavwp6z4b2vviqpl0vdxxa5skcs1s-herdr-supervisor-0.1.0`. Logs: `.direnv/service-setup-nix-build.log`, `.direnv/service-setup-nix-check.log`. The new read-only diagnostic is not part of that gate.

`control setup-local --spec FILE --bundle DIR` prepares same-host controller/node state without starting services or models. The controller and node remain separate SQLite authorities, using the existing enrollment mutation and signed NATS delegation. The frozen intent and authority fingerprints precede enrollment/publication; public output contains paths, DIDs and argv rather than tokens. Explicit `--provision` uses the separate administrator broker credentials. Repository/native health and authenticated model readiness are not inferred from a generated bundle.

Six focused regressions pass (10.74 s), followed by strict all-target Clippy (27.55 s), in `.direnv/service-setup-focused-final.log`:

| Evidence | Boundary established |
| --- | --- |
| `local_setup_replays_one_identity_enrollment_and_consumable_private_bundle` | Stable keys/enrollment/output on repeat, private file modes, actual config parsers, no tasks/process sockets and no owner-key copy to the node |
| `local_setup_recovers_enrollment_commit_before_failed_token_publication` | SQL rollback before enrollment, committed enrollment before publication conflict, same operation/key/token on retry, and reconstruction of a missing config without changing the signed grant |
| `local_setup_refuses_changed_inputs_files_credentials_and_retired_node` | Frozen capacity/specification, retained operator edits, changed active owner-token fingerprint and retired-node refusal |
| `local_setup_rejects_aliases_shared_broker_credentials_foreign_bundle_and_symlinks` | Existing-ancestor path aliases, separate broker credential files, unrelated directory content, symlink output preservation and setup lock contention |
| `local_setup_bounded_specs_and_expired_grants_make_no_authority` | Bounded linked declarative specification and explicit expiry with no controller/bundle creation |
| `local_setup_refuses_credential_role_collisions_before_creating_state` | Mailer admin/sender separation and refusal of external broker credentials pointing at generated node tokens or controller authority files |
| `local_setup_does_not_adopt_a_replaced_validly_signed_grant` | A replacement with another ID or epoch is rejected even when the owner signature is valid; missing grant exports are recreated byte-for-byte from the frozen operation/time |
| `local_setup_provisions_real_broker_only_explicitly_and_retains_bundle_on_failure` | Real restricted NATS: offline preparation creates no consumer, runtime account cannot provision, admin setup/repeat succeeds, both runtime accounts bind their consumers, and failures preserve the bundle |

The actual CLI regression uses paths with spaces, retries an unavailable broker credential, preserves node tokens, refuses changed input and checks stdout/stderr for credential leakage. Both continuous service workflows now create the node/grant/configurations through `setup-local --provision`, repeat it offline and start services using its returned argv. Their existing real Git/NATS/IPC/process, native-identity, memory, review, integration/artifact and shutdown assertions remain. The notifier is configured, but these workflows still use simulated native models and no authenticated coordinator. The pre-grant-review suite passed 554 unit tests (46.99 s), 11 CLI tests (24.47 s) and strict Clippy (29.67 s), plus both final formatted workflows independently (23.80 s). Logs: `.direnv/service-setup-final.log`, `.direnv/service-setup-continuous.log`, `.direnv/service-setup-fmt.log`. Review then identified that the saved grant export needed to be checked against the frozen operation/time, not merely its valid signature/scope; the final suite passes with that correction: 555 unit tests (45.73 s), 11 CLI tests (25.40 s) and strict all-target Clippy (57.21 s), in `.direnv/service-setup-grant-final.log`. The complete workflows use only the generated service configurations and returned startup argv. Initial compilation exposed a Rust `PathBuf`/`Path` function-pointer coercion; it was corrected before the focused tests. Nix reported a transient ignored evaluation-cache busy message, not a test failure. The completed package gate is recorded above.

### Native Herdr subscriptions and reconnection

Commit `fcc3f07aef0a3d964ee6759aeb99bd5cf1b4cc3c` completes its x86_64-linux package build and flake check with 547 unit tests (129.96 s) and 10 CLI tests (47.06 s) offline. Output: `/nix/store/1dqz5mk76v99xqc1yvc9j7n4iqriq6xw-herdr-supervisor-0.1.0`. Logs: `.direnv/herdr-events-nix-build.log` and `.direnv/herdr-events-nix-check.log`. The newer onboarding changes are not included in this gate.

`herdr_events.rs` subscribes through a dedicated Unix connection before requesting `session.snapshot` on another connection. `service.rs` wires its coalesced hints into node and coordinator-launch reconciliation. Endpoints are derived from explicit executor configuration and active saved plans. Event payloads do not write SQLite or grant resource authority. The local Herdr 0.9.1 `api schema --json` reports protocol 22; only bundled metadata was read, with no request to a shared live server. The schema snapshot used during implementation is `.direnv/herdr-0.9.1-schema.json`.

Seven focused tests pass (`.direnv/herdr-events-focused-final.log`, 1.15 s), followed by strict all-target Clippy (23.44 s):

| Evidence | Boundary established |
| --- | --- |
| `partial_frame_survives_cancel_and_keeps_its_original_deadline` | Consumed bytes and the initial completion deadline survive cancellation; an expired partial frame fails |
| `subscription_precedes_snapshot_and_partial_event_survives_bootstrap` | Real private Unix connections, subscription ACK before snapshot request, partial event spanning bootstrap, and loss error propagation |
| `frames_bound_bytes_and_reject_eof_malformed_and_wrong_ack` | Exact frame boundary, oversized/unterminated/malformed input, mismatched ACK identity, envelope validation, dotted/snake names and ignored future events |
| `absent_named_pane_falls_back_to_lifecycle_and_unknown_protocol_fails_closed` | Whole-request pane rejection retries lifecycle-only subscription and fresh snapshot; protocol 21 refused |
| `watchers_reconnect_resnapshot_coalesce_and_close_owned_streams` | Loss-triggered real reconnection and fresh snapshot, 1,000-event burst, one pending hint, minimum wake interval and owned-stream shutdown |
| `changed_targets_abort_old_stream_and_limits_are_explicit` | Removed endpoint closes the old subscription; 64-socket/256-pane caps report excess targets for periodic reconciliation |
| `target_discovery_uses_only_active_saved_plans_and_explicit_configuration` | Prepared and launched checkpoint layouts, retired resource exclusion and saved coordinator endpoint/pane discovery |

The continuous CLI workflows now expose a separate native-protocol fixture and check known-pane subscription, false exit plus `events_lost`, a fresh empty snapshot after reconnection, unchanged live attempt/epoch/resource/capacity and closed subscriptions after shutdown. Git, NATS, wrappers and service processes are real; Herdr events/snapshots and models remain fixtures. This is not authenticated model or live Herdr acceptance. The final full suite passes 547 unit tests (41.71 s), 10 CLI tests (24.80 s) and strict all-target Clippy (26.64 s), including the final adjustment preserving periodic reconciliation after subscription-target discovery failures. Logs: `.direnv/herdr-events-final.log`; the preceding full run also passed (`.direnv/herdr-events-full.log`). `nix fmt` formats the Python fixture (`.direnv/herdr-events-fmt.log`). Its completed package gate is recorded above. The first focused compilation failed only on a missing test collection type annotation; it was corrected before the passing focused run.

The worker-navigation checkpoint `e87b17f9b75f96cd1ad6b98dcbe2a250057cd40b` passes strict all-target Clippy (33.35 s, `.direnv/worker-focus-full.log`), `nix fmt`, package build and `nix flake check` on x86_64-linux. The package ran 540 unit tests (93.27 s) and 10 CLI tests (52.41 s), including the previous popup viewport failures. Output: `/nix/store/96q5p1gfkgxnd1l4vzrqiwvyqxvifxjg-herdr-supervisor-0.1.0`; logs: `.direnv/worker-focus-nix-build.log`, `.direnv/worker-focus-nix-check.log`. The later native-event gate is recorded above.

### Requirement audit, 2026-10-05

This audit reads the fourteen deliverables in `IMPLEMENTATION.md` together with the acceptance cases retained in `supervisor-design.md` and `projects-assessment.md`. An implementation or a mocked boundary test is not proof of a live runtime/remote-host acceptance case. The goal remains open.

| Deliverable | Current source/evidence inspected | Remaining completion evidence or work |
| --- | --- | --- |
| 1. Projects adaptation and isolation | `paths.rs` uses `.config/herdr-supervisor`, `.herdr-supervisor` and `HERDR_SUPERVISOR_ROOT`; `NOTICE`, plugin manifest, managed forms and original CLI/UI remain present | Final packaged plugin workflow, including operator navigation/setup |
| 2. Adoption fixes | `coordinator_review.rs` covers unknown remote branches, per-project polling and corrupt-inventory refusal; package wrapper/build identity are explicit | Remote-host acceptance matrix and final package gate; mocks do not establish SSH compatibility |
| 3. SQLite authority | `schema.sql` constraints; `store.rs` immediate transactions, active-principal authentication before replay, request fingerprint, journal and outbox; concurrent/DAG/migration regressions | Final audit of all required replan/lifecycle cases; green capacity tests alone do not close the whole scheduler |
| 4. Durable effects | Node/controller outboxes and inboxes, `executor.rs` saved intents, process receipts, recovery and lost-response regressions | Explicit recovery of still-uncertain effects and terminal outgoing records; retain occupied capacity until evidence exists |
| 5. Identity/enrollment | `identity.rs` Ed25519 did:key records; separate node keys/grants and current origin/epoch authorization; `node_lifecycle.rs` owner drain/resume and drained-node revocation | Final lifecycle gate, key rotation/replacement acceptance and remote operational evidence; application revocation does not revoke broker credentials or immediately invalidate a disconnected daemon's cached grant |
| 6. Private NATS | `transport.rs` persists application acceptance before ACK; generated node/controller ACLs and real restricted-broker tests | Final package gate and actual multi-host execution evidence |
| 7. Herdr/native workers | Exact persisted resource binding, runtime wrappers, native mailer operation/recipient preconditions and worker-ready capability; event/reconnection tests over private Unix sockets and continuous node services | Authenticated new-session Codex/Claude/Pi acceptance chain and live native Herdr/capability acceptance; Unix protocol fixtures alone do not prove these |
| 8. Mailer client extension | Separate mailer commit `7a12d5a`, prior independent build/check evidence and supervisor replay tests | Preserve the versioned client/server contract in final integration |
| 9. API/CLI/MCP and lifecycle | Controller/worker boundaries, pause/resume/cancel, repair and registration/passive-exit recovery; bounded pages/receipts/record reads | Remaining uncertain-effect recovery and full acceptance audit; no forced abandonment is implemented |
| 10. Verification/integration/cleanup | Immutable results, combined candidates, independent check/review gates and recoverable signed verifier receipts; exact Git CAS; cancelled repository release; explicit worker-checkout retirement with local operator attestation, retained refs and uncertain-result reconciliation | Verification/coordinator resource retirement and unattended process containment; an operator attestation does not prove detached descendants exited automatically |
| 11. Curated memory | Proposal provenance, revision/withdrawal, bounded topic snapshots and transactional live-memory NATS updates; memory-specific regression suite | Final package gate and live runtime consumption evidence; delivery is not a read acknowledgement |
| 12. Shared UI authority | Managed binding/facade scope, legacy-command fencing, asynchronous pages and captured revisions; task/profile/memory forms; exact worker navigation through owner-configured routes; resumable local service bundle exercised through both continuous workflows | Native service readiness/onboarding and full native/remote interaction evidence remain open |
| 13. Nix/package/docs | Blueprint `prefix=nix`, locks, package resources/wrapper, devshell/formatter, opt-in modules and module-evaluation tests | Latest package/check gate and packaged smoke; no host activation is authorized |
| 14. Acceptance evidence | Real SQLite/Git/IPC/process/NATS tests and explicit boundary limits in this document | Latest offline gate, authenticated runtime matrix, remote/native acceptance and unresolved mandatory cases above |

Priority is the missing end-to-end operator/lifecycle path: service onboarding, recovery/cleanup and native acceptance evidence. Project-wide memory across runs, Markdown memory exports, embedded large-record editing and automatic control-plane failover are extensions; they are not substitutes for closing the required acceptance cases. The initial design's `events.subscribe`/reconnection behavior is now implemented in `herdr_events.rs` and wired into continuous node/coordinator services, with private protocol and continuous node tests as recorded above. The authenticated runtime matrix still needs direct evidence.

The full Nix gate for `64751dde1497588a644cde90958bfbb947cd1bca` failed with **533/534 unit tests**: `paginated_task_and_profile_rows_open_editors_without_aggregate_run_context` still drew against ambient terminal dimensions and did not render `Original title` in the builder. The two remaining test render calls now use the same explicit 120×30 viewport as the earlier corrected popup test; assertions remain. Its chained flake check did not run. Logs: `.direnv/operator-reads-nix-build.log`. A later successful gate is required.

The large-record checkpoint passes strict all-target Clippy (35.45 s, `.direnv/record-reads-full.log`) and `nix fmt` with no changes (`.direnv/record-reads-fmt.log`). Its full Nix build/check remains pending; the failed package gate for `64751dd` predates these record reads.

The bounded operator/receipt checkpoint also passes strict all-target Clippy (39.89 s, `.direnv/operator-reads-full.log`) and `nix fmt` (no changes, `.direnv/operator-reads-fmt.log`). Its full Nix build/check remains pending.

**Last completed full Nix gate:** commit `01d20d8cb932fda69ce599f2386251d88492621e`, with 591 unit and 15 CLI tests offline on x86_64-linux, as recorded above. It includes cancellation renewal, expired node-event recovery, node lifecycle, checkout-retention inspection, local diagnosis/onboarding, event subscriptions/reconnection and earlier pagination/navigation changes. Verifier completion recovery is newer; this gate does not establish other platforms or completion of the remaining feature scope.

The worker-navigation checkpoint passed **540 unit tests and 10 CLI tests** (local suite with four test threads), including the inherited Projects suite; later checkpoint results are recorded above. The controller tests use private temporary state; real Git, SQLite, sockets, native child processes, a pseudoterminal and isolated NATS brokers are exercised. Runtime tests replace the model executables and Herdr/mailer boundaries with controlled fixtures.

Worker navigation checkpoint: `.direnv/worker-focus-full.log` records **540 unit tests (45.09 s)** and **10 CLI tests (25.19 s)**. `managed_worker_focus_uses_configured_routes_and_rechecks_exact_native_identity` exercises local/remote route argv and exact session/runtime/workspace/pane/terminal/cwd checks, ambiguity, epoch mismatch and retirement during observation through the real controller API. `managed_focus_routes_are_owner_configured_bounded_and_bound_to_the_run` covers owner-only route updates, scope, malformed/oversized/symlink inputs and binding mismatch. `managed_worker_focus_key_captures_attempt_epoch_and_refuses_stale_rows` checks the actual popup key path and frozen epoch. Herdr responses are fixtures; no live host, machine configuration or native session is changed. The initial focused runs exposed a self-conflicting authority lock and an inappropriate private-daemon-directory requirement; routing now uses its own regular-file lease while the Project view retains its authority lease. The final focused run passes these regressions plus the viewport-independent editor test (`.direnv/worker-focus-fixed.log`).

Large-record reads: `.direnv/record-reads-full.log` records **537 unit tests (45.08 s)** and **10 CLI tests (25.84 s)**. `record_tests` reconstructs the full public JSON through alternating real IPC/MCP reads, verifies exact offsets/hash/Unicode/escapes, rejects changed content and malformed UTF-8 positions, rechecks scope/retirement and covers named policy keys and private launch credential exclusion. The existing actual CLI workflow reconstructs a large record through both `managed record` and `control record`. The oversized pagination regression now proves explicit references advance to subsequent records; Projects renders references without mutation actions. The popup exposes retrieval details, not an embedded full large-record editor. SQLite projection construction and per-chunk whole-record hashing remain costs. The package gate for these changes remains pending.

The subsequent package gate for `cf3ba46553272f74ad566a148a740b5be1aed938` failed: 525/526 unit tests passed; `paginated_panel_keys_send_exact_cursors_and_memory_history_has_no_approval_action` failed its rendered `approved` assertion. The renderer read ambient terminal dimensions; this regression now supplies a fixed 120×30 viewport, retaining the cursor/status/action assertions. The corrected test passes locally. That failed build did not reach CLI tests or flake check and is not a completed gate. Log: `.direnv/project-pages-nix-build.log`.

Bounded operator reads and receipt checkpoint: `.direnv/operator-reads-full.log` records **534 unit tests (47.63 s)** and **10 CLI tests (26.19 s)**. `operator_reads_tests` covers authorized profile selection, unresolved launch/error preview, exact active resources and malformed observation batches. Its real Unix API test commits an operation whose legacy response exceeds 4 MiB, recovers its hash/receipt, reopens SQLite after another decision and verifies identical replay; foreign scope and journal failure cannot return a committed receipt or leave partial changes. `receipt::tests` covers encoded JSON boundaries, escapes and full-result hashing. The large-run pagination fixture also reads coordinator opening through real IPC. CLI coverage exercises bounded default status, explicit full status, pages and receipt recovery. Internal original-response allocation/storage and oversized individual-record retrieval remain limitations. No models or live Herdr servers were used for this checkpoint.

| Requirement | Evidence | Limit |
| --- | --- | --- |
| Atomic capacity, DAG/revisions and operation replay | `src/control/tests.rs`: concurrent dispatch, dependency cycles, retired supervisors and transaction rollback; candidate tests schedule a reviewer while the author occupies the worker slot | Broader replanning/abandonment of an uncertain verifier pending |
| Unknown remote branch, inventory corruption and project polling | `src/coordinator_review.rs` plus inherited suite | Managed projection now tested separately; automated onboarding still pending |
| Signed origin and route authorization | Identity tampering/expiry tests, controller receive tests, real NATS ACL tests | Explicit node rotation/revocation commands remain pending |
| Reply inbox confidentiality and broker management | `real_broker_generated_acl_isolates_reply_inboxes_and_runtime_management` requires actual NATS permission errors for foreign reply subscriptions, consumer reads, stream mutations, message reads and ACKs; confirms legitimate deliveries/ACKs/reconnects | Generated runtime policy must be applied to the deployed broker; administrators remain trusted |
| Broker policy usability and reserved routes | `broker_routes_reject_reserved_names_and_wildcards_before_enrollment_or_grant`; continuous CLI workflow invokes `broker-policy` and installs its permissions on separate controller/node users | Example in `docs/nats-acl.md`; existing host/broker configuration is not changed |
| Durable offline transport | Real JetStream restart/offline, expired node-event recovery and cancellation renewal tests; ACK follows SQLite acceptance | Other expired controller commands and invalid original repair remain pending |
| Poisoned consumer windows | `poisoned_broker_windows_cannot_starve_valid_commands_and_events`: 65 malformed records per endpoint followed by a valid command/event, real restricted NATS clients, zero broker pending deliveries | TERM follows local persistence, not application acceptance; bounded storage can still exhaust |
| Failed local quarantine write | `failed_local_hold_never_terminates_broker_delivery` injects SQLite failure on both endpoints and observes one outstanding broker delivery each | No TERM, command or applied inbox row is written |
| Deferred prerequisites, restart and lost local receipt | `missing_assignment_retries_after_restart_and_lost_application_receipt`; `deferred_controller_application_survives_restart_and_creates_one_receipt` | Real broker/SQLite; no models; retry runs full original authorization |
| Incoming integrity, limits and maintenance | `reception_checks_integrity_time_scope_and_never_renews_or_leaks_records`; `reception_limits_fail_closed_and_oversize_documents_retain_no_body`; continuous CLI workflow uses status/prune | 4,096 rows, 64 MiB raw bodies, no automatic terminal eviction; pending entries survive pruning |
| Deferred execution fencing | `frozen_reception_bytes_and_cancellation_fence_deferred_execution` | Changed stored bytes rejected; cancellation tombstone prevents delayed launch |
| Controller/node schema 6 | `schema_five_upgrade_preserves_existing_jobs_and_frozen_outbox`; earlier migration tests updated to include the new table | Existing event identities, frozen outbox and jobs preserved |
| Durable retry and lost SQL publication receipt | `broker_acceptance_with_lost_sql_receipt_retries_identical_bytes_after_restart` injects failure after real broker storage, reopens SQLite and checks unchanged wire/ID, broker dedup and controller application | Network timeout alone never establishes acceptance |
| Expired, malformed and premature outgoing records | `expired_malformed_and_future_outgoing_records_do_not_block_commands_or_receipts` keeps expired/invalid rows, defers a future timestamp and preserves active process capacity | Outgoing terminal states do not establish task outcomes |
| Fairness across denied routes | `denied_outgoing_route_does_not_starve_incoming_events_or_other_nodes` receives a worker proposal and delivers its receipt while another destination is denied by real NATS ACLs | Incoming consumer/service outages remain separate failures |
| Bounded retry schedule and private inspection | `retry_schedule_survives_restart_is_bounded_and_operator_projection_omits_credentials`; continuous CLI workflow invokes `delivery-status` | At most 100 recent metadata rows, with full state counts; expired node events have explicit recovery, other terminal states remain retained |
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
| Missing receipt / stale PID | `missing_exit_receipt_never_signals_a_saved_pid_or_frees_capacity` points the stored PID at a separate live test process | Legacy or insufficient identity evidence remains occupied; passive Linux recovery is covered separately below |
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
- Recovery of still-live orphans, legacy capture gaps, changed boots/namespaces and non-Linux wrapper loss; detached-descendant audit and conservative resource cleanup.
- Reconciliation for legacy/lost-wrapper registration evidence. Explicit recovery covers prelaunch Claude/Pi and still-owned Codex before binding.
- Native/remote acceptance of the implemented service bundle, dependency diagnosis and cross-host focus routes. Managed creation, local onboarding and focus have fixture coverage.
- Final acceptance of bounded Projects pages and oversized-record retrieval. Those features are implemented; legacy full snapshots and internal aggregate response allocation retain their documented limits.
- Expired execution/memory command handling, invalid original repair and in-place node-key rotation. Expired node events and cancellation delivery have explicit recovery; current-enrollment grants and owner drain/revocation are implemented.
- Stronger model-facing validation beyond the isolated continuous-service workflow and protocol fixtures.

Project/cross-run memory and Markdown exports are optional extensions, not remaining requirements for this goal.

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

## Passive kernel exit recovery

The preceding Codex recovery commit `737eb14` passed its complete x86_64-linux package/flake gate: 500 unit tests (89.15 s), 10 CLI tests (46.15 s), build phase 9 min 42 s and check phase 6 min 8 s. Output: `/nix/store/f02dzl6s9fb0c564va1d5l5wmd7lbi4h-herdr-supervisor-0.1.0`. Logs: `.direnv/codex-registration-nix-build.log` and `.direnv/codex-registration-nix-check.log`. Other systems were not built.

The new recovery regressions exercise the following boundaries:

| Invariant | Regression | Scope |
| --- | --- | --- |
| Kernel identity, live process, exit, reused identity and foreign boot/namespace | `kernel_process_identity_distinguishes_live_exit_reuse_and_foreign_scope` | Real child/pidfd; identity mismatch simulates PID reuse and leaves the live process untouched |
| Child ownership despite inherited auto-reaping | `native_wrapper_retains_child_when_sigchld_would_otherwise_auto_reap` | Isolated wrappers start with SIGCHLD ignored or SA_NOCLDWAIT; fast-exiting children retain captured identity and their actual code 7 |
| Correct birth-field parsing | `kernel_process_parser_uses_birth_field_after_full_command_name` | Commands containing spaces and parentheses; malformed records rejected |
| Ownership and exact row/epoch required | `process_recovery_requires_exact_identity_and_abandoned_private_owner` | Both node and coordinator tables, live lease even after child exit, missing/corrupt identity/lease, foreign boot and ordinary receipt preservation |
| Node recovery and durable signed stop replay | `native_owner_crash_releases_lease_without_claiming_its_child_exited` | Real wrapper killed through its test Child handle; native process exits separately; SQL faults before local receipt and after signed event, restart and controller-only capacity release |
| Coordinator recovery and transactional retirement | `coordinator_recovers_crashed_wrapper_only_after_native_exit_and_retries_retirement` | Real wrapper/native process, failed principal retirement, restart, one journal event and no repeated native launch; Herdr/mailer are fixtures |
| Private regular ownership file | `native_ownership_lease_requires_live_holder_and_rejects_missing_symlink_or_public_file` | Also rejects a FIFO without blocking |

Recovered exit status remains unknown, even when the fixture deliberately exits with code 7. These tests do not establish detached-descendant cleanup, active orphan termination, another platform or model processing. The final local suite passes 505 unit tests (38.82 s) and 10 CLI tests (23.67 s), with four test threads. `nix fmt` passes without source changes. Logs are `.direnv/kernel-recovery-final.log` and `.direnv/kernel-recovery-fmt.log`; the earlier pre-SIGCHLD suite is retained in `.direnv/kernel-recovery-full.log`. Strict all-target Clippy passes (26.18 s). The full package gate remains pending.

## Transactional signed-record size limits

The controller now checks the complete command envelope before committing each remote outbox intent. Both author dispatch and independent reviewer reservation roll back on an oversized combination of separately valid inputs. A shared constructor keeps preflight and later signing aligned, reserving the largest epoch/timestamp widths. Signing and verification use the same 262,144-byte record budget; the transport retains its 300,000-byte wire limit. No data is silently truncated.

| Invariant | Evidence | Scope |
| --- | --- | --- |
| Oversized combined contract/memory consumes no capacity or capabilities | `wire_limits_dispatch_rolls_back_capacity_credentials_and_all_effects` | Legal 65,536-byte instructions plus criteria and a 32,000-byte memory selection with six-byte JSON escapes; explicit withdrawal permits the same dispatch operation to succeed once |
| Reviewer reservation is equally atomic | `wire_limits_review_reservation_rolls_back_assignment_and_capacity_before_retry` | Real Git verification, owner profile update, memory, failed review reservation and exact retry without an orphaned reviewer assignment |
| Exact JSON record boundary | `wire_limits_signing_exact_encoded_boundary_and_signature_bound` | ASCII, multibyte UTF-8 and six-byte escapes at the exact record limit and one byte over; malformed oversized signatures rejected before base58 decoding |
| Node outbox never persists an oversized new event | `wire_limits_node_rejects_oversized_event_before_durable_enqueue_and_allows_retry` | A command that fits the mutation budget but exceeds the signed-event envelope; no stored event or consumed operation key |
| Near-limit valid execution still works | `wire_limits_large_execute_roundtrip_over_real_broker_preserves_full_contract` | More than 250,000 encoded bytes over restricted real NATS; exact contract/memory after node restart and ACK after persistence; no model execution |
| Envelope and future numeric widths included | `wire_limits_command_budget_includes_future_epoch_and_full_json_envelope` | Exact command-preflight boundary, one byte over, larger epoch before signing, exact wire limit and excess whitespace rejection |

The final local suite passes 511 unit tests (42.68 s), 10 CLI tests (25.35 s), all six focused size regressions and strict all-target Clippy (30.44 s), with four test threads. `nix fmt` passes. Logs: `.direnv/wire-limits-final.log` and `.direnv/wire-limits-fmt.log`. The full Nix gate remains pending. These tests do not establish aggregate API pagination or recovery of previously oversized terminal deliveries.

## Bounded coordinator context pages

`control context` and `supervisor_context` now return a bounded overview with counts/cursors; `control page` and `supervisor_page` read complete records from sixteen authorized collections. Policy maps are separate, so accumulating profiles does not inflate the overview. Each read authenticates within its SQLite snapshot. Cursors bind owner, run, principal and collection; policy pages additionally reject a changed run revision. Ordinary scans bound appended rowids and report current-per-page consistency, not frozen history. No cursor grants authority or acknowledges a notice.

The regressions `paginated_context_preserves_all_large_tasks_and_separates_growing_policy_maps`, `pagination_cursors_recheck_scope_authority_limits_and_policy_revision`, `pagination_reads_current_updates_but_new_rows_require_a_new_scan`, `pagination_collections_preserve_provenance_withdrawal_and_hide_capabilities` and `pagination_oversized_legacy_record_fails_without_partial_json` cover a context above 4 MiB, all 32 full contracts without duplicates, growing profiles, retired/foreign/worker credentials, bounded cursors, concurrent-between-page changes, explicit withdrawal, proposal evidence, assigned-profile snapshots and credential-free coordinator summaries. The large-context case also runs the real Unix API and MCP facade. `pagination_legacy_memory_access_remains_inside_worker_or_reviewer_assignment` also covers the legacy snapshot/memory boundary, own-topic reads, unrelated-topic refusal, reviewer scope and retirement. The existing native-attachment CLI test invokes the new `page` command; the API/MCP regression reads a node through its returned collection cursor.

Response regressions `api_response_encoder_accepts_exact_frame_and_rejects_one_more_byte`, `api_response_client_requires_complete_bounded_json_line` and `mcp_response_overflow_returns_one_complete_error_and_keeps_next_request` check exact encoded limits, escaped expansion, a real socket's truncated/oversized response and MCP stream continuity. These paths use mocked model/Herdr/mailer boundaries. Pages do not solve oversized individual records or the legacy Projects view's aggregate query. The final local suite passes 520 unit tests (37.16 s), 10 CLI tests (23.83 s) and strict all-target Clippy (16.82 s), with four test threads; log: `.direnv/pagination-keyset-final.log`. Numeric collections compare and order raw rowids rather than formatted cursor strings; SQLite query-plan inspection confirms an integer-primary-key range scan for tasks. The package gate for this checkpoint remains pending.

The prior recovery commit `35ccfef` completed its x86_64-linux gate with 505 unit tests (115.33 s) and 10 CLI tests (48.35 s); build phase 13 min 17 s, check phase 10 min 2 s. Output: `/nix/store/sbwfybpb156d5bfm3cc166whga4z44h3-herdr-supervisor-0.1.0`. Logs: `.direnv/kernel-recovery-nix-build.log` and `.direnv/kernel-recovery-nix-check.log`. Other systems were not built.

The signed-record budget commit `27ed1ae` completed its x86_64-linux Nix build/flake gate with 511 unit tests (114.74 s) and 10 CLI tests (47.93 s). Output: `/nix/store/040nd0m2l7gwmbksfi7imwfqmnj5lkrs-herdr-supervisor-0.1.0`. Logs: `.direnv/wire-limits-nix-build.log` and `.direnv/wire-limits-nix-check.log`. Other systems were not built.

## Bounded Projects picker

The picker now reads `CoordinatorOverview` independently from panel details, sharing the same four-reader pool. `summary_and_detail_share_capacity_but_never_share_payloads_or_old_authority` covers coalescing, one global limit, distinct payloads, cross-projection rebinding and corrupt-marker invalidation. The large-context regression now exercises the actual `Binding.overview` and default asynchronous cache over real IPC when legacy context exceeds 4 MiB, checking all 32 tasks are counted without downloading contracts. The gated-popup test answers summary and detail requests separately while preserving the selected record, filter and captured stop confirmation. This changes data retrieval, not terminal layout or key bindings. Detail-panel pagination remains pending. The follow-up passes 521 unit tests (32.91 s), 10 CLI tests (22.40 s), strict all-target Clippy (13.19 s) and `nix fmt`. Focused cache/popup/large-context tests also pass. Logs: `.direnv/picker-overview-focused.log`, `.direnv/picker-overview-full.log`, `.direnv/picker-overview-fmt.log`. Its full package gate remains pending.

## Paginated Projects panels

Threads, Inbox and Memory now use an authenticated `project_page` projection with a header and one collection read in the same SQLite transaction. The initial page cursor is returned and retained, preserving the scan boundary across refresh/back navigation until Home restarts. Pages use the existing 50-record/1-MiB collection limit with a 3-MiB projected response limit. Picker, legacy panels and page reads share four background slots. A page change fences late replies; the per-collection history retains 64 previous positions. Tasks, Routines, Settings, full-context CLI flows and oversized individual records remain pending.

| Evidence | Boundary established |
| --- | --- |
| `project_memory_pages_keep_full_history_and_current_approval_revisions` | 67 proposals, complete 32,000-byte UTF-8 texts across multiple pages, provenance, withdrawn revision and exact revision-2 approval; history gives no approval action for an approved proposal; reads do not write journal decisions |
| `project_pages_recheck_credentials_and_cursors_and_never_offer_stale_actions` | Exact attempt/profile snapshot, worker/node denial, principal/collection cursor scope, no leaked capability and stale read-only rendering |
| `project_page_start_cursor_keeps_scan_boundary_until_explicit_restart` | A later append changes the total but not the original scan; restart includes it; loading/failed panels retain navigation without decisions |
| `page_navigation_rejects_late_refresh_and_recovers_from_failed_next_page` | Late old-page response cannot overwrite a failed/new page; previous preserves its initial cursor; restart resets it; exact binding and end-of-list checks |
| `paginated_panel_keys_send_exact_cursors_and_memory_history_has_no_approval_action` | Real Unix socket, actual popup keys for next/previous/Home/list switching, frozen confirmation revision, rendered history/hints and zero submitted decisions; controller replies are protocol fixtures |
| Extended `context_drains_unacknowledged_notices_across_multiple_pages` | All 106 pending notices are visible without an implicit acknowledgement |
| Extended large-context IPC regression | Actual Binding page requests remain usable while the full context exceeds 4 MiB; unrelated tasks are not downloaded |

The final local suite passes 526 unit tests (40.46 s), 10 CLI tests (24.83 s), strict all-target Clippy (24.23 s) and `nix fmt`. Focused tests also pass. The full package gate remains pending. Logs: `.direnv/project-pages-first.log`, `.direnv/project-pages-focused.log`, `.direnv/project-pages-final.log`, `.direnv/project-pages-fmt.log`. The earlier full-suite attempt stopped on an incorrect test-module import, corrected before the final run. Model/Herdr/mailer execution remains simulated.

The prior pagination commit `352dcec` completed its x86_64-linux Nix gate with 520 unit tests (98.50 s) and 10 CLI tests (46.97 s). Output: `/nix/store/hdqmjhkxllfynphy0l0z6pnam2ghx97f-herdr-supervisor-0.1.0`. Logs: `.direnv/pagination-nix-build.log` and `.direnv/pagination-nix-check.log`.

## Paginated task/profile editors and remaining panels

All six managed popup sections now use page projections. Task records include their complete predecessor IDs in the same read snapshot; forms capture authorized check names and a small run scope rather than deserializing a partial policy as a complete Run. Off-page dependency IDs are checked authoritatively by the controller. Profiles use their own page, the global count controls removal, and `create_profile` prevents a new-profile form from silently replacing a hidden name. The new owner-only mutation shares revision, run scope, operation replay and journal transactions; its receipt is compact. Existing `set_profile` responses and synchronous full-context CLI reads retain their aggregate limits.

| Evidence | Boundary established |
| --- | --- |
| `task_page_edit_preserves_off_page_dependencies_and_new_tasks_validate_them_authoritatively` | 61 tasks across pages, predecessor outside the visible 50, full amendment/replay, new task referring to another page, missing predecessor rollback and self-dependency refusal |
| `profile_page_creation_cannot_replace_a_hidden_name_and_edits_capture_page_revision` | 51 profiles, a single-profile last page still permits removal when others exist, hidden-name rejection, edit/revision fencing, stale cursor refusal and compact idempotent creation receipt |
| `create_profile_is_owner_scoped_and_rolls_back_with_its_audit_and_operation` | Supervisor/foreign-run denial, injected journal failure rolls back profile, revision and operation; exact retry succeeds once through the Projects API |
| `paginated_task_and_profile_rows_open_editors_without_aggregate_run_context` | Actual popup key handling and rendered forms from headers without profile/check maps; off-page predecessors and exact argv retained; no controller decisions submitted |
| Extended native-coordinator profile notice regression | `create_profile` produces one coordinator notice, including after an operation replay |
| Extended real CLI workflow | `create_profile` through `managed apply`, stable replay receipt and collision rejection beside legacy `set_profile` behavior |
| Extended large-context IPC regression | Every project collection remains bounded while the legacy context exceeds 4 MiB |

Focused tests pass, followed by all 530 unit tests (47.53 s), 10 CLI tests (26.44 s) and strict all-target Clippy (38.59 s). `nix fmt` also passes (`.direnv/editor-pages-fmt.log`); the full package gate remains pending. Logs: `.direnv/editor-pages-first.log`, `.direnv/editor-pages-focused.log`, `.direnv/editor-pages-full.log`. The initial panel regression still expected Tasks to be unsupported; that assertion was updated for the new supported endpoint. Model, Herdr and mailer boundaries remain simulated.
