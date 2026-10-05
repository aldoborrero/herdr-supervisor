# Managed supervisor operations

This guide covers the new `control` commands. The [inherited Projects guide](upstream/operations.md) describes a separate file-based scheduler. Managed UI projection and protection against legacy command bypass are not finished: do not manage the same work through both schedulers.

## Processes and authority

Run `control run-controller --broker BROKER_JSON --repositories REPOSITORIES_JSON` against one private controller state directory. It combines the API, relay and Git verifier. `control serve` and `control relay-controller` expose the first two components independently; they do not execute checks or integration intents. Each execution node has a different private directory, its own signing key, and a `control run-node` process combining its relay, worker API and executor. The node's SQLite inbox/outbox survives process restarts. The separate mailer daemon must already be running locally on that node.

State paths are explicit. No startup command edits global agent configuration, installs Herdr plugins or restarts shared sessions. Service modules and a complete onboarding command remain pending.

| File or directory | Owner and purpose |
| --- | --- |
| Controller `control.sqlite` | Tasks, attempts, decisions, journal, memory, delivery intents |
| Controller `owner.key` | Owner signing authority; never copied to nodes |
| Controller `admin.token` | Local owner capability; never given to workers |
| Controller `control.sock` | Private controller API |
| Node `node.key` | Delegated node signing key |
| Node `node.sqlite` | Commands, jobs, resource checkpoints and outgoing event receipts |
| Node `worker.sock` | Scoped worker API |
| Node `workers/<attempt>/` | Worker credential, mailer registration/config and native receipts |
| Mailer state | Separate daemon database and credentials; never opened as a supervisor database |

Directories must be private (0700), credentials regular private files (0600). These are trusted same-UID components. They do not provide isolation from malicious processes running as the same Unix user. Keep secret files outside repositories and Nix store paths.

## Create a run

Initialize the controller, then submit this shape with `control --state DIR apply --credential DIR/admin.token --operation OPERATION_ID --request FILE`. Replace the base with a full commit OID and the repository with a stable identifier/path mapped on the node.

```json
{
  "method": "create_run",
  "params": {
    "project": "demo",
    "repo": "/repos/demo",
    "base_oid": "0123456789012345678901234567890123456789",
    "objective": "Implement the agreed parser changes",
    "policy": {
      "max_active": 3,
      "reserved_reviewers": 1,
      "max_attempts": 3,
      "duration_secs": 3600,
      "profiles": {
        "implementation": { "runtime": "codex", "argv": [], "limit": 2 }
      },
      "checks": { "tests": ["cargo", "test", "--locked"] },
      "require_review": true
    }
  }
}
```

The API returns a run ID and revision. `bind_supervisor` with that run and `expected_revision` issues a run-scoped credential; persist it privately. Rebinding retires the previous supervisor credential. Mutating decisions use explicit aggregate revisions; retry the *same* operation ID and content after an uncertain response. Reusing an ID with different content fails.

The create-run operation records a logical repository identity. Node preparation and controller verification resolve it through their respective owner-supplied mappings. Controller verification binds the canonical Git common directory and refuses concurrent authority through a second alias. The owned integration branch must not preexist on first binding.

## Enroll a node and provision NATS

1. Run `control --state NODE_DIR node-key` on the node. Copy only its public DID to the controller.
1. Submit `enroll_node` with `id`, `key` and `max_active` using the controller owner credential. Save the returned node capability privately on that node.
1. Issue `control --state CONTROLLER_DIR grant NODE_ID NODE_DID --epoch 1 --expires-at UNIX_SECONDS`. Transfer this signed JSON grant to the node. Pin the expected owner DID independently.
1. Configure a private NATS server with JetStream and credentials scoped to each node. Broker provisioning needs an administrator credential; runtime nodes must not have it.
1. Run `control --state CONTROLLER_DIR provision --broker BROKER_JSON --target NODE_ID`. It provisions the owner stream, controller consumer and named node consumers.
1. Start `run-controller --broker BROKER_JSON --repositories REPOSITORIES_JSON`. Start the node command below.

Broker JSON contains `url`, `credentials` (an absolute private `.creds` or JSON user/password file), and optional `private_network_plaintext`. TLS is required by default. Plaintext requires an explicit private-network choice. NetBird addresses can provide reachability without changing record authorization.

Subjects are `hs.<sha256-owner-DID>.to.<node-id>` and `.to.controller`. Consumers use explicit ACK after SQLite application. Grant expiry and node epoch are checked on reception. Automated grant renewal and node rotation/revocation commands are not yet provided.

A node's broker ACL must prevent publishing to other nodes' command subjects and reading their consumers. It needs publication to the controller subject, access to its own durable pull consumer, scoped ACK publication and reply inbox subscriptions. The real-broker authorization test demonstrates deny-by-default routing; a production ACL template is still pending.

## Run an executor

Executor configuration maps controller repository identities to authorized local checkouts:

```json
{
  "repositories": { "/repos/demo": "/home/user/src/demo" },
  "base_remotes": { "/repos/demo": "controller" },
  "worktree_root": "/home/user/.local/state/herdr-supervisor/worktrees",
  "herdr_binary": "/absolute/path/to/herdr",
  "herdr_socket": "/absolute/path/to/herdr.sock"
}
```

`base_remotes` is optional. If an assigned base/candidate is absent locally, the node fetches that exact OID from the configured Git remote into an owned input ref. This is needed for reviewers on a different host. Configure Git access separately; NATS transports assignments, not Git objects.

Runtime configuration uses private credential **paths**:

```json
{
  "supervisor_binary": "/absolute/path/to/herdr-supervisor",
  "mailer_state": "/home/user/.local/state/herdr-mailer",
  "mailer_admin_credential": "/private/mailer-admin.token",
  "node_credential": "/private/node.token",
  "codex_binary": "/absolute/path/to/codex",
  "mailer_runtime_binary": "/absolute/path/to/herdr-mailer-runtime",
  "claude_development_channel": false
}
```

`mailer_admin_credential` contains the raw mailer admin token, not `owner.json`. It remains with the node executor. Enable `claude_development_channel` explicitly to use Claude; the mailer runtime adds the development-channel flag without bypassing tool permissions.

```bash
herdr-supervisor control --state NODE_DIR run-node \
  --node NODE_ID --owner OWNER_DID --grant grant.json \
  --broker broker.json --executor executor.json --runtime-config runtime.json
```

This command can create Herdr workspaces and start real models for dispatched jobs. Use `--once` to run one reconciliation cycle; it is **not** a dry run. For inspection, use controller snapshots or read-only worker context instead.

Low-level `prepare`, `launch`, `relay-node` and `serve-workers` commands expose the same components independently. Do not run a separate relay against a directory already owned by `run-node`.

Profiles accept model selection and a limited set of runtime tuning flags. Session, root, config/profile switching and permission overrides are reserved to the launcher. Codex uses the installed CLI's `--approve-for-me --sandbox workspace-write`; this requires a CLI supporting those options. See [official CLI reference](https://learn.chatgpt.com/docs/developer-commands?surface=cli) and the local `codex --help` when updating supported versions.

## Worker lifecycle and recovery

1. Dispatch reserves capacity and records an immutable contract/memory snapshot.
1. The node persists a worktree plan, including the exact path, branch and base commit, before asking Herdr to create it.
1. Git and Herdr observations must agree on repository, branch, checkout, workspace, pane and terminal. A reused terminal or occupied pane fails reconciliation.
1. A one-shot wrapper launches the runtime. Codex's UUID is observed on that exact pane; Claude/Pi receive a preassigned UUID.
1. The node registers the recipient with mailer, then asks the controller to bind the complete resource identity.
1. After the controller application receipt, the node sends an idempotent bootstrap message with exact mailer recipient session/epoch preconditions.
1. The model calls `worker_ready`. Only this functional response transitions the controller attempt to `running`.

A command reply, broker ACK or runtime API submission never completes steps 6–7 by itself. The worker CLI and MCP expose receipts with `queued`, `broker_stored` and `controller_applied` states.

Lost create/launch replies do not reset the effect marker. Existing resources can prove success; absence leaves uncertainty. A lost mailer **registration** response without a durable local receipt remains unresolved; idempotent registration/reconciliation is a remaining gap. Send retries already use mailer operation IDs.

Cancellation increments the attempt epoch and retires its capability. It fences further worker decisions and node launch/preparation. Automatic process stop, verification that a native model turn has stopped, and cleanup are **not implemented yet**. Cancellation therefore keeps capacity reserved until an authorized node submits a confirmed `observe_stopped`. Never infer stopped from an absent pane alone.

## Verify, review and integrate code

Controller repository configuration authorizes both the local checkout and optional sources for commits produced by remote nodes:

```json
{
  "repositories": {
    "/repos/demo": {
      "path": "/home/controller/src/demo",
      "node_remotes": { "worker-a": "worker-a" }
    }
  },
  "check_timeout_secs": 300
}
```

Git remote names/URLs come from this owner configuration, never from worker output. The controller requires the submitted commit to descend from its assigned base. It combines that commit with the run's current integration base using Git's merge machinery, preserving both parents in a deterministic candidate. Conflicts become explicit `conflict` records without modifying the author's or user's checkout.

Use the authenticated `apply` API/CLI/MCP for these decisions:

1. `request_verification`: `attempt`, `expected_revision` of the **task**. The controller service builds the candidate and runs the contract's named checks using argv approved in the run policy.
1. `request_review`: `integration`, its current `expected_revision`, `node`, `profile`. When review is required and checks pass, this reserves a separate reviewer attempt with its own worktree/session. Reserved review capacity is available even when the author still occupies a worker slot.
1. The assigned reviewer sends `submit_review`: `attempt`, `epoch`, `integration`, `candidate_oid`, `contract_hash`, `approve`, `criteria` and `findings`. Each criterion needs its zero-based `criterion` index, `passed` boolean and nonempty `evidence`. Rejection requires actionable findings. The author cannot approve its own candidate.
1. `apply_integration`: `integration` and its current `expected_revision`. The controller revalidates review/check receipts and advances `refs/heads/hs/integration/<run>` only if its old OID equals the verified base. It refuses a checked-out or symbolic integration ref.

Checks run in a controller-owned detached checkout under `verification/<integration>/checkout`. Receipts retain exact candidate/tree, policy hash, argv, resolved executable and hash, environment digest, timeout, exit status and output hashes/excerpts. Logs are private. An author saying tests passed does not create a check receipt. A check that changes tracked files fails; a started check without a durable outcome is not rerun automatically.

For one explicit reconciliation, the owner can use `control verify --credential ADMIN_TOKEN --repositories REPOSITORIES_JSON INTEGRATION_ID`. This is not a dry run. `run-controller --once` runs one service cycle. Continuous mode keeps IPC and transport active while checks execute and rotates pending candidates so an unavailable repository does not monopolize selection.

If Git advances but the SQLite receipt fails, a later reconciliation recognizes the exact candidate at the owned ref and records the existing effect. It does not apply it a second time. Keep the controller's integration refs reserved for this service: independent Git clients do not participate in its SQLite lock.

This pipeline currently supports `code` contracts. Artifact verification, automated repair/replanning, replacement of failed reviewers, release of repository bindings at run completion and conservative worktree cleanup remain pending. A submitted review or integrated task does not prove that either runtime has stopped.

## Memory

Workers submit `propose_memory` with `run`, `topic`, `text`, `source_attempt` and `evidence`. The supervisor applies `approve_memory` with the proposal ID and expected topic revision. Withdrawal appends a new revision. Read via `control memory` or the authenticated API/MCP.

Memory is run-scoped and includes provenance; only requested topics plus preferences enter an assignment, bounded to 32,000 UTF-8 text bytes. Cross-run project memory, Markdown projections and delivery of later memory changes to already-running workers remain pending.

## Backups

Use SQLite's online backup mechanism or stop the owning processes before copying a complete state directory. Copying only a live `.sqlite` file can omit WAL transactions. Back up owner/node keys and credentials privately with their corresponding database; do not rotate them merely to recover a process crash.
