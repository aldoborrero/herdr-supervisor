# Validation and completion status

Reviewed 2026-10-05. This document separates implemented/tested behavior from the full delivery contract in [IMPLEMENTATION.md](../IMPLEMENTATION.md). Passing the current suite does not establish completion of the supervisor.

## Evidence collected

The local implementation suite currently passes **362 unit tests and 5 CLI tests**, including the inherited Projects suite. The controller tests use private temporary state; real Git, SQLite, sockets and isolated NATS brokers are exercised. Runtime tests replace the model executables and Herdr/mailer boundaries with controlled fixtures.

| Requirement | Evidence | Limit |
| --- | --- | --- |
| Atomic capacity, DAG/revisions and operation replay | `src/control/tests.rs`: concurrent dispatch, dependency cycles, retired supervisors and transaction rollback; candidate tests schedule a reviewer while the author occupies the worker slot | Replacement of failed reviewers and repair attempts pending |
| Unknown remote branch, inventory corruption and project polling | `src/coordinator_review.rs` plus inherited suite | Managed UI is not connected to the controller |
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

- Artifact verification without a synthetic code commit; repair/replanning and replacement of failed reviewers.
- Release of controller repository bindings at run completion and conservative verification checkout cleanup.
- Confirmed native stop, uncertain-process recovery and conservative cleanup.
- Idempotent/reconcilable mailer registration after a lost registration response.
- Managed Projects UI/actions and prevention of legacy scheduler bypass.
- NATS backoff/expiry handling, node lifecycle operations and deployment ACL examples.
- Live memory change consumption and optional Markdown projection.
- Opt-in NixOS/Home Manager modules and their evaluation tests.
- A complete isolated workflow through the continuous node service, plus stronger model-facing validation.

No push, public release, host changes or deployment has occurred. Existing user sessions and shared runtime servers were not modified.
