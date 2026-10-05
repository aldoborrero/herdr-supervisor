# Managed supervisor operations

This guide covers the new `control` commands. The [inherited Projects guide](upstream/operations.md) describes a separate file-based scheduler. Managed UI projection and protection against legacy command bypass are not finished: do not manage the same work through both schedulers.

## Processes and authority

Run a controller API (`control serve`) and controller relay (`control relay-controller`) against one private controller state directory. Each execution node has a different private directory, its own signing key, and a `control run-node` process combining its relay, worker API and executor. The node's SQLite inbox/outbox survives process restarts. The separate mailer daemon must already be running locally on that node.

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

The current create-run operation does not canonicalize or verify a controller-side Git path. Node repository authorization and preparation verify the actual local Git checkout. Repository aliases and controller integration still need completion.

## Enroll a node and provision NATS

1. Run `control --state NODE_DIR node-key` on the node. Copy only its public DID to the controller.
1. Submit `enroll_node` with `id`, `key` and `max_active` using the controller owner credential. Save the returned node capability privately on that node.
1. Issue `control --state CONTROLLER_DIR grant NODE_ID NODE_DID --epoch 1 --expires-at UNIX_SECONDS`. Transfer this signed JSON grant to the node. Pin the expected owner DID independently.
1. Configure a private NATS server with JetStream and credentials scoped to each node. Broker provisioning needs an administrator credential; runtime nodes must not have it.
1. Run `control --state CONTROLLER_DIR provision --broker BROKER_JSON --target NODE_ID`. It provisions the owner stream, controller consumer and named node consumers.
1. Start the controller API and `relay-controller --broker BROKER_JSON`. Start the node command below.

Broker JSON contains `url`, `credentials` (an absolute private `.creds` or JSON user/password file), and optional `private_network_plaintext`. TLS is required by default. Plaintext requires an explicit private-network choice. NetBird addresses can provide reachability without changing record authorization.

Subjects are `hs.<sha256-owner-DID>.to.<node-id>` and `.to.controller`. Consumers use explicit ACK after SQLite application. Grant expiry and node epoch are checked on reception. Automated grant renewal and node rotation/revocation commands are not yet provided.

A node's broker ACL must prevent publishing to other nodes' command subjects and reading their consumers. It needs publication to the controller subject, access to its own durable pull consumer, scoped ACK publication and reply inbox subscriptions. The real-broker authorization test demonstrates deny-by-default routing; a production ACL template is still pending.

## Run an executor

Executor configuration maps controller repository identities to authorized local checkouts:

```json
{
  "repositories": { "/repos/demo": "/home/user/src/demo" },
  "worktree_root": "/home/user/.local/state/herdr-supervisor/worktrees",
  "herdr_binary": "/absolute/path/to/herdr",
  "herdr_socket": "/absolute/path/to/herdr.sock"
}
```

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

## Memory

Workers submit `propose_memory` with `run`, `topic`, `text`, `source_attempt` and `evidence`. The supervisor applies `approve_memory` with the proposal ID and expected topic revision. Withdrawal appends a new revision. Read via `control memory` or the authenticated API/MCP.

Memory is run-scoped and includes provenance; only requested topics plus preferences enter an assignment, bounded to 32,000 UTF-8 text bytes. Cross-run project memory, Markdown projections and delivery of later memory changes to already-running workers remain pending.

## Backups

Use SQLite's online backup mechanism or stop the owning processes before copying a complete state directory. Copying only a live `.sqlite` file can omit WAL transactions. Back up owner/node keys and credentials privately with their corresponding database; do not rotate them merely to recover a process crash.
