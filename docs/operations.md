# Managed supervisor operations

This guide covers `control` and the managed Projects view. The [inherited Projects guide](upstream/operations.md) describes the file-based scheduler used by projects that have not been attached to a controller. Attached projects read controller records and reject legacy scheduling commands.

## Processes and authority

Run `control run-controller --broker BROKER_JSON --repositories REPOSITORIES_JSON` against one private controller state directory. It combines the API, relay and Git verifier. `control serve` and `control relay-controller` expose the first two components independently; they do not execute checks or integration intents. Each execution node has a different private directory, its own signing key, and a `control run-node` process combining its relay, worker API and executor. The node's SQLite inbox/outbox survives process restarts. The separate mailer daemon must already be running locally on that node.

State paths are explicit. No startup command edits global agent configuration, installs Herdr plugins or restarts shared sessions. Opt-in [NixOS and Home Manager service modules](services.md) wrap these commands. Project creation is available through `managed create` and the popup form; service provisioning remains explicit. See [getting started](getting-started.md).

| File or directory | Owner and purpose |
| --- | --- |
| Controller `control.sqlite` | Tasks, attempts, decisions, journal, memory, delivery intents |
| Controller `owner.key` | Owner signing authority; never copied to nodes |
| Controller `admin.token` | Local owner capability; never given to workers |
| Controller `control.sock` | Private controller API |
| Controller `coordinators/<run>/<generation>.token` | Run-scoped coordinator capability; retired when replaced |
| Controller `coordinators/<run>/<generation>/` | Owned coordinator home, mailer token/config and durable registration receipt |
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

## Connect a native coordinator

The coordinator currently runs on the controller host and uses its private API. Execution nodes can be remote over NATS. This command attaches an **already registered** Codex, Claude or Pi session in the local mailer; Projects can instead request an owned coordinator using the launcher configuration below.

Enable the durable notifier by adding `--coordinator-config coordinator.json` to `run-controller`. Its configuration contains absolute paths:

```json
{
  "mailer_state": "/private/mailer",
  "sender_credential": "/private/supervisor-notifier.token",
  "supervisor_binary": "/absolute/path/to/herdr-supervisor"
}
```

The sender credential is a registered mailer agent/session token, **not** the mailer admin token. Use a dedicated sender. Mailer must support protocol-2 idempotent sends and exact recipient preconditions.

With the controller API running, bind the receiving native session:

```bash
herdr-supervisor control --state CONTROLLER_DIR connect-supervisor \
  --credential CONTROLLER_DIR/admin.token \
  --runtime-credential /private/coordinator-mailer.token \
  --mailer-state /private/mailer \
  --operation connect-coordinator-1 --expected-revision RUN_REVISION RUN_ID
```

The receiving credential is also a registered mailer session token. The command reads its exact agent, mailer session/epoch, runtime and native UUID through mailer's `doctor`, then atomically creates a new supervisor generation and bootstrap notice. It prints the private controller credential **path**, never the token. Retry with the same operation ID and unchanged registration after a lost response. A changed registration requires an explicit new binding, not reuse of the old operation.

The coordinator receives a context command pointing to that run's capability file. `control --state CONTROLLER_DIR context --credential CAPABILITY_FILE RUN_ID` and the `supervisor_context` MCP tool return a bounded objective/policy overview and collection cursors. `control page` and `supervisor_page` expose complete tasks, attempts, dependency edges, node capacity, review/integration state, profiles, approved memory, memory proposals and notices. Instructions describe how to schedule through the scoped API. No global MCP configuration is installed.

Notice intent and journal cursor commit together in SQLite. Before delivery, the notifier freezes content, destination, sender owner/agent and operation ID; retries preserve them after a lost mailer response or SQL receipt. Results, review/check outcomes, stopped attempts and memory decisions generate notices. They carry references rather than executing agent-authored text. Changing the logical sender blocks retry until the original sender is restored, because mailer scopes operation IDs by owner/author. Sender session/token rotation under the same owner/agent preserves that scope. Preserve the same authoritative mailer database; replacing it with an empty or stale copy loses its deduplication receipts. Only notices eligible for delivery contact mailer; an idle notifier needs no daemon connection.

`mailer_stored` means mailer accepted persistence for the exact recipient. It does not establish native delivery or model processing. The coordinator explicitly submits `acknowledge_notices` with `run` and `ids` after inspecting context; an owner acknowledgement identifies the owner separately. The overview includes the unacknowledged count. Scan `pending_notices` to inspect the oldest outstanding notices, up to 100 records per page, and acknowledge inspected IDs separately. Following a cursor does not acknowledge anything or approve a task/result. The legacy context endpoint retains its 100-notice view; the Projects Inbox now pages through pending notices and history.

Replacing the coordinator revokes the old capability and retires its pending notices. Retired notices keep their original destination; an already accepted or in-flight send cannot be undone. In particular, `superseded` does not prove the old recipient never received it. New generations receive a fresh bootstrap and current SQLite context.

Coordinator CLI/MCP context uses the paginated path below. The Projects picker and all six popup sections use bounded projections. The legacy snapshot API and synchronous full-context CLI commands still use their full projection. This integration has isolated IPC/broker tests with mocked mailer replies, not proof of a live coordinator model consuming its bootstrap.

## Read large coordinator contexts

```bash
herdr-supervisor control --state CONTROLLER_DIR context \
  --credential CAPABILITY_FILE RUN_ID
herdr-supervisor control --state CONTROLLER_DIR page \
  --credential CAPABILITY_FILE --collection tasks --limit 50 \
  --cursor CURSOR_FROM_CONTEXT_OR_PREVIOUS_PAGE RUN_ID
```

The overview's `collections` map gives counts and starting cursors. Read each page's `items[].value`, then follow `next_cursor` until it is null. Omit `--cursor` to start a new scan with current bounds. MCP exposes the same operation as `supervisor_page` with `run`, `collection`, optional `cursor` and optional `limit`. The default limit is 50; valid limits are 1–100. Do not interpret an unqueried collection as empty.

| Collections | Contents |
| --- | --- |
| `tasks`, `attempts`, `results` | Contracts, exact assigned profiles, resource/state records and immutable results |
| `integrations`, `checks`, `reviews` | Candidate, check and independent-review evidence |
| `dependencies`, `nodes` | Task edges and public execution capacity |
| `profiles`, `policy_checks` | Owner-approved policy maps, separated from the overview |
| `memory`, `memory_proposals`, `pending_memory` | Current topic revisions including withdrawals, proposal provenance/history, and pending decisions |
| `notices`, `pending_notices` | Current-generation notices or only those awaiting acknowledgement |
| `coordinator_launches` | Public launch/process/registration-recovery summaries; no launcher credentials |

A page returns `cursor` for refreshing that same position and `next_cursor` for advancing. The overview and each page are independently consistent SQLite read transactions. They are **not one frozen snapshot**. A rowid high-water mark bounds ordinary scans; newly appended records require a new scan. Updates, removals and replaced dependency edges may change between pages. Profile/check maps instead require the captured run revision to remain current. Refresh context and re-read relevant revisions before deciding; mutation revision/epoch checks remain authoritative. Cursors are bounded positions, not capabilities: every request reauthenticates the current owner/supervisor and checks run, principal and collection scope. Retired or worker/node credentials cannot use these pages. The legacy whole-run snapshot now also requires owner/supervisor authority. A worker/reviewer using the controller memory endpoint can read only its exact attempt contract's topics plus preferences; role, run, epoch and contract checks share the memory read transaction.

A page contains at most 1 MiB of encoded JSON. Records up to 768 KiB are inline in `items[].value`; larger projections return `value:null` and an explicit `record` reference containing byte count, SHA-256 and a starting cursor. The item still advances the page position. Nothing is silently skipped or interpreted as an empty record.

Use `control record --credential FILE --cursor CURSOR RUN`, `managed record SLUG --cursor CURSOR`, or MCP `supervisor_record` with `run` and `cursor`. Each returns at most 32,768 bytes of UTF-8 JSON text in `text`, together with byte `offset`/`end`, whole-record `bytes`/`sha256` and `next_cursor`. Concatenate text in exact offset order until the cursor is null, verify the final byte count/hash, then parse the JSON. A fragment need not itself be valid JSON. Every request reauthorizes the principal/run and hashes the complete current public projection in one read transaction. A changed record, including a same-length change, requires discarding the partial result and obtaining a fresh page reference. Repeating a cursor against unchanged data returns the same fragment. Cursors grant no authority and may not cross principals/runs. The same collection projections keep launch credentials and other private columns out of both inline and chunked reads.

This bounds IPC fragments and Rust copies, not SQLite's projection construction or hashing work: each chunk hashes the complete projection. No persistent snapshot/cache is created by these reads. The private API and MCP cap serialized response lines at 4 MiB, and the client rejects oversized or incomplete lines. Legacy oversized queries receive a complete error directing the caller to pagination. If an oversized response follows a mutation, do not infer that it rolled back or resubmit with a new operation ID; preserve the exact retry identity and recover its receipt. Legacy aggregate construction/storage remains separate from chunked record reads.

## Launch a coordinator from Projects

Keep `run-controller --coordinator-config coordinator.json` running with the notifier configuration above. The controller runs the coordinator launcher independently of the Git verifier and notifier. Herdr and mailer must already be available on the controller host. Create a launch configuration containing absolute paths:

```json
{
  "herdr_binary": "/absolute/path/to/herdr",
  "herdr_socket": "/private/herdr.sock",
  "supervisor_binary": "/absolute/path/to/herdr-supervisor",
  "mailer_state": "/private/mailer",
  "mailer_admin_credential": "/private/mailer/admin.token",
  "codex_binary": "/absolute/path/to/codex",
  "mailer_runtime_binary": "/absolute/path/to/herdr-mailer-runtime",
  "claude_development_channel": false
}
```

The launcher and notifier must name the same mailer state. The launcher uses mailer admin authority only to register its receiver; the native child receives that receiver's credential, its generation-scoped controller capability and explicit run context. No inherited worker attempt/epoch or mailer context is passed through. Set `claude_development_channel` to `true` explicitly when choosing a Claude profile. Codex uses Auto with `workspace-write`; Pi and Claude use mailer's packaged runtime bridge.

Attach the operator view with an **owner** credential and add `--launch-config /absolute/path/to/launch.json`. Then run:

```bash
herdr-supervisor --root PROJECTS_DIR open demo --profile implementation
```

Choose a profile from the run policy, not the legacy Projects profile store. If the policy has only one profile, `--profile` is optional. This submits an owner-only `start_coordinator` mutation with the displayed run revision and a stable operation ID. It does not launch inside the popup or invoke the legacy scheduler. The same mutation can be submitted through `control apply` with `run`, `expected_revision`, `profile` and `config` fields.

SQLite reserves one generation before external effects. An owner mutation that races a launch effect returns a retryable busy error before changing state; retry the same operation ID and content after the launcher releases its lease. Controller paths are normalized before generating commands for the child workspace. Herdr creates a separate workspace rooted in the coordinator's private home; workers still receive isolated Git worktrees. The generated `AGENTS.md` directs the coordinator to scoped context. The one-shot wrapper owns its native process. Codex's UUID is observed on the exact pane; Claude/Pi receive a UUID before launch. The controller verifies workspace, pane, terminal, runtime and UUID, then activates that generation's mailer route and durable bootstrap. `bound` and `model_ready: false` report resource binding without claiming model processing.

`managed status` and the popup's Settings section expose launch state, errors and process exit receipts. Reopening a pending launch reports its state. Lost workspace or pane-run replies trigger inspection without repeating creation or launch. A lost mailer registration response stays uncertain. The local owner can explicitly authorize one retry with [registration recovery](#recover-a-lost-registration): before Claude/Pi launch, or for an exactly observed, still-owned Codex before receiver binding. A missing wrapper exit receipt stays unresolved unless the passive Linux recovery below establishes the original native process has exited. No stored PID is signalled during recovery.

Run cancellation, deadline expiry or generation retirement stop a process still owned by its wrapper. To replace an owned coordinator without cancelling the run, the owner can submit `bind_supervisor` for the current run revision; this retires the old generation. Wait until its launch state is `exited`, then open again. `--new` cannot override an active or unresolved coordinator. Existing manually attached sessions are not process-owned by this launcher. Resource deletion and stopping a still-live orphan remain pending.

## Create a managed project and view

`managed create --spec FILE` (or `--spec-json JSON`) records an immutable creation intent, submits an owner-authorized `create_run`, and publishes an already-bound Projects view. The popup's **N** form submits the same specification. See [getting started](getting-started.md) for fields and examples. Neither path starts a coordinator or workers.

The private `<PROJECTS_DIR>/.managed-setup/<slug>/intent.json` preserves the normalized inputs, stable operation ID, credential fingerprint and local receipts. Publication uses atomic no-replace rename on Linux/macOS; the old scheduler never sees the staged project. Destination conflicts preserve existing content. Interrupted staging directories remain private and retained.

Use `managed resume-setup SLUG` or form **Ctrl-R** with the saved name after a lost response. A changed credential or specification is rejected before replay; rotating authority during an unresolved setup requires explicit operator reconciliation. `managed setup-status SLUG` shows the local receipt only, with `current_controller_state_verified: false`. It does not query the controller or prove that the published view still exists. `managed status SLUG` queries the bound run after publication.

## Attach the Projects view

Create a fresh project folder, create a controller run whose `project` field equals its slug, and attach it **before** using the inherited `open` command:

```bash
herdr-supervisor --root PROJECTS_DIR new demo
herdr-supervisor --root PROJECTS_DIR managed attach demo \
  --controller CONTROLLER_DIR --credential CONTROLLER_DIR/admin.token --run RUN_ID
herdr-supervisor --root PROJECTS_DIR popup demo
```

`managed attach` validates access through the running controller API. `.state/managed.json` stores the run, controller directory and credential path, never a raw token. An owner or run-scoped supervisor capability can be used for the operator's view. A supervisor credential is retired when that coordinator is replaced; reattach the same run with its new credential path to restore access. Credentials remain private and outside the project repository. Native coordinators use their own generation-scoped credential from the mailer bootstrap, not the operator view's credential; the generated `AGENTS.md` makes that distinction explicit.

Attachment requires no legacy threads, recorded coordinator or task entries; it does not migrate existing work. A shared authority lock spans legacy operations, including their external calls. Attachment takes the exclusive lock and refuses while an operation holds it. The binding is persisted before `AGENTS.md` is replaced, so a failed priming write still blocks the old scheduler; repeat attachment to repair that view. Changing the bound run/controller or silently detaching is not supported yet.

`list`, `overview`, `context`, the project picker and popup use the controller for attached projects. If the controller is unavailable or the binding is malformed, they report that failure instead of displaying legacy task or memory files. Legacy thread/routine/settings/delete/rename commands and ticker processing cannot load a managed project. Delayed rename notifications are fenced too. Files and locks are trusted same-UID state, not isolation from a process that deliberately edits them.

`managed status SLUG` and `context SLUG` return the bounded run overview, including collection counts and cursors. Read records with `managed page SLUG --collection tasks --limit 50`; pass its `next_cursor` as `--cursor` until null. The sixteen collection names are listed by `managed page --help`. `managed status SLUG --full` explicitly requests the legacy aggregate snapshot and can fail the IPC size limit on a large run. Text `overview` shows only the first bounded task page and labels a partial list.

Coordinator opening reads the current route, first unresolved launch and selected authorized profile name without downloading task/history/profile bodies. Multiple profiles require an explicit selection. Reservation still rechecks the captured revision and launch occupancy. Launch errors above 16,384 UTF-8 bytes are explicitly omitted from this preview with their byte count; omission never makes an unresolved launch eligible for replacement. Workspace detection compares batches of at most fifty observed sessions against active controller bindings, including the exact worker runtime/workspace/pane/terminal.

Popup decisions and forms submit `managed apply SLUG --receipt ...`. This returns a bounded `operation_receipt`: `committed` means the SQLite transaction succeeded, not that a runtime executed the decision. The receipt contains the operation ID, SHA-256 and encoded byte count of its original JSON result, plus a small summary. Results up to 65,536 encoded bytes are also included; larger ones have `response_inline:false` and `response:null`. Inspect current records through pages. Retrying the same operation and command returns the original receipt even after newer decisions or restart. A lost/oversized legacy reply can be recovered with the same operation ID and `--receipt`. Authorization is checked on every retry. Plain `managed apply` retains its legacy full result; storing and constructing that result internally is still unbounded by this receipt mechanism.

| Popup section | Controller data and actions |
| --- | --- |
| Threads | Attempts, role, node and state; `s` requests stop and `f` focuses an active bound session using the displayed epoch |
| Tasks | Create a task with Enter/`n` on its creation row; `e` edits a planned contract/dependencies; integration/check/review evidence; `r` requests repair of interrupted/rejected tasks |
| Inbox | Native coordinator notices and their receipts; `a` acknowledges an inspected notice |
| Settings | Objective, policy, nodes, coordinator and launch receipts; `p` pauses/resumes, `x` cancels; owner can create profiles, `e` edit and `d` remove |
| Memory | Approved topics with versions and pending proposals with evidence; `a` approves, `w` withdraws |
| Routines | Displays the controller scheduling policy; legacy routines are not executed |

The Tasks creation row opens a form for an active run. It captures that run's revision, policy and dependency index; refresh cannot replace them while the form is open. Criteria/checks/topics/dependencies use JSON lists, result type selects code or artifact, and the controller remains responsible for authorization and transactional validation. The form freezes the serialized request and operation ID before submission, sends JSON over stdin, and retains both after a lost or failed response. Enter retries it; Escape returns to the list without undoing any accepted operation. Stale views cannot open this form. See [task entry](getting-started.md#add-a-task-from-the-popup).

Enter shows the selected record. Destructive decisions and memory changes display the captured record for confirmation. Requests keep the displayed revision/epoch and a stable operation ID; stale decisions fail rather than silently applying to newer state. The `project_apply` API checks run membership and applies/replays the operation in one transaction, including already-decided memory proposals. It cannot use an owner view credential to act on another run or enroll nodes.

The equivalent CLI is `managed apply SLUG --operation ID --request JSON_FILE` (`-` reads stdin). `managed status SLUG` and `context SLUG` return current controller context. Existing `pause` and `resume` plugin actions use that same authority. `open SLUG` and the popup's `c` key focus an existing coordinator only after observing its exact native UUID/runtime and matching terminal in the selected Herdr server. If no coordinator is bound or pending, an owner view with `--launch-config` can reserve a new coordinator. A missing registered session never triggers an automatic replacement.

The popup and its project picker read controller state in the background, with at most four simultaneous reads shared by picker summaries and detail panels, and one pending read per project binding/projection. The picker uses the bounded coordinator overview and server-side task counts; it does not download task contracts, results or memory. All six sections load one collection page at a time, including task/profile editors captured from the current page. An initial view shows a loading state. Refresh failure retains the last successful snapshot as explicitly stale/read-only; action hints and coordinator launch are disabled there. A changed or malformed binding invalidates the cached authority, and replies for an older binding are discarded. Reconnection restores current data and actions. This cache lives only in the popup process and never writes controller decisions or legacy project files.

Background refresh preserves the selected managed record, project-picker filter and selection, and open confirmations/details. A confirmation retains its captured revision/epoch, which the controller still validates. An explicit mutation uses the existing CLI/API path and may wait for its response; only background reads are asynchronous. `managed status`, `context` and `overview` remain synchronous CLI reads. Worker focus uses explicit node routes as described below; actual cross-host validation, service onboarding and internal aggregate snapshot bounds remain pending. The popup itself must run on the controller host; workers may execute remotely over NATS.

## Browse managed pages

For worker navigation, configure a route for each execution node using the owner-bound Projects view:

```json
{
  "nodes": {
    "local": { "socket": "/run/user/1000/herdr/default.sock" },
    "worker-a": {
      "socket": "/run/user/1000/herdr/default.sock",
      "machine": "saved-machine-id"
    }
  }
}
```

```bash
herdr-supervisor --root PROJECTS_DIR managed focus-config demo --file focus-routes.json
herdr-supervisor --root PROJECTS_DIR managed focus demo ATTEMPT_ID --expected-epoch EPOCH
```

The socket is explicit Herdr client context; `machine` selects an already configured Herdr remote machine through the inherited adapter. Node IDs come from controller enrollment, not display labels or cwd matching. Saving requires owner authority and writes only this project's `.state/focus.json`, bound to its controller/run. It does not enroll machines, change NetBird or launch processes. An empty `nodes` map disables configured routes. The regular input file and saved configuration are limited to 64 KiB and 128 routes; symlinks, relative sockets and invalid machine/node identifiers are refused.

Press **f** on an active, bound Threads row to invoke the same command. It queries the current active attempt/epoch, observes the exact native UUID/runtime/workspace/pane/terminal and matching worktree on that route, then rechecks the controller binding before `agent focus`. Missing routes, ambiguous sessions, changed identities, retired attempts or stale popup state fail without a focus call. The Herdr adapter does not provide an atomic UUID-compare-and-focus operation: this is verified navigation followed by a pane focus, not a lock on subsequent native session changes. It sends no model input and starts no agent. Actual remote-server interaction still needs its acceptance run; current automated tests simulate Herdr while using the real controller API.

In every managed section, use `]` for the next page, `[` for the previous page, and **Home** to restart from the first page. Navigation is asynchronous and shares the four-reader limit with the project picker. The page heading shows the page number, records shown and the current total. A page contains at most 50 complete records or explicit large-record references and may contain fewer to fit its byte budget; records are never truncated. Each panel response is capped at 3 MiB and reads its header, records and approval revisions in one SQLite transaction.

Press **v** to switch lists within a section. Tasks offers tasks, dependencies, integrations, checks, reviews and results. Settings offers profiles, nodes and coordinator launch records. Routines lists policy-approved checks and their exact argv. Inbox offers pending notices and notice history. Memory offers approved topics (including explicit withdrawals), pending proposals and proposal history. Approved proposals have no approval action. An approval still captures the current topic revision and requires confirmation; a page read never acknowledges a notice or approves memory.

Every scan preserves its starting rowid boundary through refreshes and next/previous navigation. Existing rows can change or disappear; this is a current view per page, not frozen history. New appended rows appear after **Home**, even when the current total has already increased. Up to 64 previous page positions are retained; Home is always available to restart older navigation. Changing the project binding or submitting a controller decision invalidates cached positions. Late responses cannot replace a newer page or change an open confirmation. On a failed page, use Home to retry or `[` to return to an earlier position. Stale/loading/failed pages cannot launch a coordinator or submit decisions.

Task editors capture the current task revision, complete predecessor IDs and authorized check names without loading every task or runtime profile. Dependency IDs may refer to any page; syntax/self-dependency checks run in the form, and the controller validates existence, same-run scope and cycles transactionally. An edited task preserves off-page dependencies unless the user changes that field. Oversized individual records appear as explicit references with no mutation actions; Enter shows their retrieval cursor/instructions. They do not block later records. Full large-record reading uses CLI/MCP; an embedded chunk reader/editor is not implemented in the popup.

Profile forms capture the run revision and selected profile page. Creating a profile uses `create_profile`, which requires an owner, a previously unused name and the expected run revision. A name existing on another page is rejected without overwriting it. Updating/removing a profile retains `set_profile`; the remove action uses the total profile count, not the size of the current page. The controller still refuses removal of the last profile. Both forms freeze the exact operation after submission; retries do not rebase or change its payload.

The synchronous full-context CLI commands and older aggregate mutation responses still need migration. In particular, `set_profile` retains its full-run response; an oversized response can occur after a committed change. Inspect current state and preserve the operation ID as described in the API limits section.

## Edit run profiles

In **Settings**, an owner view can select **Create a run profile…** (Enter/`n`), press **e** on an existing profile, or **d** to confirm removal. A run-scoped supervisor view displays profiles read-only. The form captures the run revision and accepts a name, Codex/Claude/Pi runtime, a JSON array of model/tuning arguments and a positive active-attempt limit. Existing names are fixed; create another profile to use another name. Runtime arguments cannot override permissions, session identity or checkout. At least one profile must remain.

Creating a new profile uses `create_profile` with `run`, `expected_revision`, `name` and a required `profile`; its receipt contains the run ID/revision and the created profile. Updates and explicit removal use `set_profile`:

```json
{
  "method": "set_profile",
  "params": {
    "run": "RUN_ID",
    "expected_revision": 7,
    "name": "reviewer",
    "profile": { "runtime": "claude", "argv": [], "limit": 1 }
  }
}
```

Submit through `managed apply` or the owner controller API with a stable operation ID. Explicit `"profile": null` removes the named profile; omitting `profile` is rejected. Changes require an active or paused, unexpired run and increment its revision. The form freezes its stdin request before submission; after a failed response Enter retries that exact operation. Escape returns to inspect state and does not undo an accepted request. Stale views cannot open editors or submit removal actions.

Changes apply to future reservations. Authors, reviewers and coordinator launch plans retain their assigned runtime/arguments even if their profile is changed or removed. The controller snapshot exposes each attempt's `profile_snapshot`. Limits count all active attempts with the same profile name, including ones reserved under earlier settings. Lowering a limit blocks new reservations while at capacity; it does not stop existing agents. The coordinator receives a profile-change notice and must reread current profiles before dispatch; its scoped credential cannot change them.

Verification candidates retain their original policy and policy hash, so editing profiles does not invalidate existing check/review receipts. Every other policy field and the exact contract still participate in the verification gate. Controller schema 8 introduced these policy snapshots and immutable attempt profiles. The current controller/node schemas are 9/8 to retain registration recovery receipts. Migration recovers profiles from original execute intents, verifies retained signed records and preserves their bytes. A missing legacy execute intent stays explicitly unavailable instead of borrowing the current profile. An old candidate whose policy no longer matches its original hash remains unresolved. No downgrade is supported; use the binaries matching the current controller/node schemas.

## Amend a planned task

The Tasks panel's **e** key opens the selected planned task with its current contract and dependency list. The form captures the task revision (not the run revision), preserves whitespace in existing instructions, sends the replacement over stdin and freezes its operation before submission. Stale views cannot open it. Escape leaves an uncertain operation intact; inspect controller state before starting a different edit.

The same scoped API/MCP decision is `amend_task`, with `task`, `expected_revision`, a complete `contract` and the full replacement `dependencies` list. The controller requires supervisor/owner authority for that run, an active run, a planned task, and no unresolved prior process/verification effects. It validates the contract against current policy, replaces edges, rejects foreign/missing/self/cyclic dependencies and updates the contract hash/revision in one transaction with its operation receipt and journal. An empty dependency list removes all predecessors. A failure preserves the original contract and edges.

Dispatch and amendment compete on the task revision. Once dispatched, the assigned contract cannot be edited through this operation; use stop/exit and the explicit retry workflow when a revision is needed. After a safe retry has returned a task to planned state, it can be amended before its next dispatch. Earlier assignment/result records and journal entries stay intact. Replaying an already accepted operation returns its historical receipt even after a later edit, without restoring old task state.

Creating or amending a task, or using the existing `add_dependency` decision, now produces a durable coordinator notice through the journal collector. This wakes the coordinator through the configured mailer path; it neither dispatches work automatically nor proves model processing. New task editing requires a controller that supports `amend_task`; older controllers reject the unknown decision.

## Enroll a node and provision NATS

1. Run `control --state NODE_DIR node-key` on the node. Copy only its public DID to the controller.
1. Submit `enroll_node` with `id`, `key` and `max_active` using the controller owner credential. Save the returned node capability privately on that node.
1. Issue `control --state CONTROLLER_DIR grant NODE_ID NODE_DID --epoch 1 --expires-at UNIX_SECONDS`. Transfer this signed JSON grant to the node. Pin the expected owner DID independently.
1. Configure a private NATS server with JetStream and credentials scoped to each node. Broker provisioning needs an administrator credential; runtime nodes must not have it.
1. Run `control --state CONTROLLER_DIR provision --broker BROKER_JSON --target NODE_ID`. It provisions the owner stream, controller consumer and named node consumers.
1. Start `run-controller --broker BROKER_JSON --repositories REPOSITORIES_JSON`. Start the node command below.

Broker JSON contains `url`, `credentials` (an absolute private `.creds` or JSON user/password file), and optional `private_network_plaintext`. TLS is required by default. Plaintext requires an explicit private-network choice. NetBird addresses can provide reachability without changing record authorization.

Subjects are `hs.<sha256-owner-DID>.to.<node-id>` and `.to.controller`. Consumers use explicit ACK after SQLite application. Grant expiry and node epoch are checked on reception. Automated grant renewal and node rotation/revocation commands are not yet provided.

A node's broker ACL must prevent publishing to other nodes' command subjects and reading their consumers or reply inboxes. Generate the exact runtime permissions with `control --state CONTROLLER_DIR broker-policy --node NODE_ID`; see the [deployment example and reply-prefix migration](nats-acl.md). The controller and each node use distinct `_HS_INBOX.<owner-hash>...` prefixes automatically. The generated policy includes only the runtime's publish routes, its own pull consumer and ACKs, and its own reply subscriptions. Broker administration is a separate credential. The continuous-service test uses the generated permissions for both the controller and node, while real-broker regressions verify rejected reads, writes and management requests.

## Signed-record size budgets

A signed record's complete JSON document is limited to **262,144 encoded bytes**; its signed wire envelope is limited to **300,000 bytes**. These are encoded sizes, not character counts. Control characters can consume six JSON bytes each. For example, a valid 32,000-byte memory selection can occupy 192,000 bytes before adding its topic metadata, task contract and assigned profile.

The controller checks every remote command inside the transaction that creates its outbox intent. It uses the same command-envelope constructor as signing and reserves the maximum numeric epoch/timestamp widths. An oversized dispatch or review request rolls back its attempted reservation, credentials, profile snapshot, assignment, journal and operation receipt. The error reports the encoded size and limit. Reduce the contract, assigned profile or selected memory explicitly before retrying; nothing is silently truncated or omitted to meet this transport budget. A profile or memory change can permit the same unapplied dispatch/review request to succeed. Amending the contract also changes its task revision, so the next dispatch must carry that current revision. Existing operation replay semantics still apply once a request succeeds.

Signing enforces the same record limit, so a node cannot persist an oversized outgoing event for publication. Verification rejects oversized records and bounds the base58 signature before decoding. Ordinary incoming wire bounds and NATS stream limits are unchanged. Local controller effects that never cross NATS are not subject to the remote-command envelope check.

These guards prevent new oversized records from becoming unpublishable reservations or queued events. They do not rewrite old outbox rows, repair previously uncertain deliveries, or provide large-run API pagination. Terminal-record recovery and aggregate API/context bounds remain tracked separately.

## Outgoing delivery and retries

Each relay reads incoming records before attempting outgoing publication. A denied destination or an expired outgoing record therefore does not prevent reading new commands/results. Outgoing selection is limited to 16 records per cycle. Due controller cancellation commands have priority over other deliveries; within that priority it is ordered by due time. Bulk artifact receipts cannot consume all positions ahead of a due cancellation. Network publication has a three-second budget for the batch and a maximum of two seconds per attempt; SQLite locking and incoming broker calls have their own timeouts.

The relay persists the attempt count and next retry time before publishing. Failures retain a delay of 2, 4, 8, 16, 32 and then at most 60 seconds, including across restarts. A process that dies during publication leaves that reservation in place. The signed bytes, message ID, author and expiry remain frozen; losing the SQLite publication receipt causes an idempotent retry, not a new operation. Broker storage is still distinct from controller application or a native/model result.

An expired outgoing record becomes `expired`; a malformed or cryptographically invalid stored node record becomes `invalid`. These states stop publication and retain the record for inspection. They do not complete a task, free process capacity or establish whether an earlier uncertain attempt reached the receiver. A frozen timestamp ahead of the local clock is deferred without rewriting it. Already accepted controller receipts remain authoritative, and malformed neighboring rows cannot block valid receipt lookup or native bootstrap.

Inspect controller deliveries using an owner credential, or a node credential scoped to the selected node:

```bash
herdr-supervisor control --state CONTROLLER_DIR delivery-status \
  --credential CREDENTIAL_FILE --node NODE_ID
```

The result contains counts for every delivery state and at most 100 recent metadata rows, including attempts, expiry, next attempt time and a diagnostic category. It omits command bodies, signed envelopes and credentials. This command is an inspection of controller state; it does not change the broker or retry a terminal record. Worker `receipt` and MCP `worker_receipt` include the same attempt/schedule/error fields for that worker's event. A zero `next_attempt_at` means no retry is scheduled after a terminal or successful delivery.

Node schema 5 adds retry metadata while preserving existing event IDs/wires, jobs, memory replicas and operation replays. Controller retry metadata uses its existing outbox columns. Expiry does not renew a grant or extend a task deadline. Recovery of terminal outgoing records and explicit grant/node lifecycle operations remain pending. Rejected incoming records follow the durable holding policy below; they never receive an application acceptance ACK.

## Rejected incoming deliveries

Controller and node schema 6 add `reception_failures` to their existing private SQLite databases. If application rejects a broker record, the relay first commits its original bytes, SHA-256, state and retry schedule locally. Only after that commit does it send JetStream `TERM` and await broker confirmation. This stops broker redelivery without asserting successful processing ([NATS acknowledgement semantics](https://github.com/nats-io/nats.docs/blob/master/using-nats/jetstream/model_deep_dive.md)). It frees the consumer's pending-delivery window so malformed records or temporarily missing prerequisites do not indefinitely block later work. Ordinary accepted records still use the application transaction followed by a confirmed ACK.

Local states are distinct:

- `pending`: a structurally intact signed record was rejected by application checks. Up to 16 due records are retried per relay cycle, before fetching new deliveries, with persisted exponential delays capped at 60 seconds. Every attempt checks the original bytes, signature, expiry, current authorization, route and epoch again. Permission failures are never bypassed. A dependency arriving later can allow an otherwise valid record to succeed.
- `invalid`: encoding, integrity or immutable envelope checks failed. No application retry is scheduled. Documents over 300,000 bytes retain only their hash and size.
- `expired`: the original signed validity elapsed. No automatic re-signing, deadline extension or inference about a past uncertain outcome occurs.
- `applied`: a retry committed the ordinary application transaction. Its receipt is separate from model processing. A crash before saving this local retry receipt replays the receiver's existing idempotent operation.

Duplicate rejected bytes reuse the same local entry, including after a lost TERM response. Diagnostic categories and status output contain no raw record bodies, capabilities or exception strings. The private database may contain credentials inside retained wire records; keep it private and preserve it together with ordinary controller/node state. A restored broker without this database cannot recover records previously transferred to the local queue by TERM.

Each endpoint retains at most 4,096 rejected records and 64 MiB of raw bodies. A full or unwritable holding store sends no TERM; the broker retains responsibility for redelivery. Existing local retries run before attempting new intake. Storage exhaustion can still halt intake and must be resolved; this is bounded recovery, not an unlimited denial-of-service guarantee.

Local operator inspection uses filesystem authority and does not contact a broker or expose message bodies:

```bash
herdr-supervisor control --state CONTROLLER_DIR reception-status
herdr-supervisor control --state NODE_DIR reception-status --node
```

Both views include state counts and at most 100 recent metadata rows. After inspecting the records, explicitly prune terminal entries older than a past positive Unix timestamp:

```bash
herdr-supervisor control --state CONTROLLER_DIR reception-prune --before UNIX_TIMESTAMP
herdr-supervisor control --state NODE_DIR reception-prune --node --before UNIX_TIMESTAMP
```

Pruning never deletes `pending` entries, command/inbox application receipts, jobs or task results. It deletes the selected terminal diagnostic records and their retained bodies; there is no automatic history eviction. It does not compact the SQLite file, requeue a terminal record or discard broker history. Normal SQLite page reuse allows subsequent entries within the logical limit. Back up the private state before intentionally removing evidence that you need to retain.

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

## Recover a lost registration

If registration reached mailer but its response was lost, the local owner can authorize one more attempt while the receiver is still unbound. Run this on the node for a worker/reviewer, or on the controller for an owned coordinator:

```bash
herdr-supervisor control --state NODE_DIR recover-registration --node \
  --operation RECOVERY_ID --expected-epoch ATTEMPT_EPOCH ATTEMPT_ID
herdr-supervisor control --state CONTROLLER_DIR recover-registration \
  --operation RECOVERY_ID --expected-epoch COORDINATOR_GENERATION LAUNCH_ID
```

This is local maintenance using access to private state, like reception pruning. It is not exposed through worker or supervisor capabilities. The command does not contact mailer, create resources or start another model. It records `registration_retry_authorized` and clears only the pending registration marker. The ordinary launcher subsequently retries registration and continues its existing binding/bootstrap sequence, preserving the native UUID, attempt/generation, worktree and assignment. A new receiver token/epoch is possible; managed launch must not have persisted or exposed the previous receiver credential.

Eligibility depends on the runtime:

- **Claude/Pi:** before native launch, with no wrapper/launch marker or process row. The receipt's `stage` is `before_native_launch`.
- **Codex:** the original wrapper must still hold its private ownership lease, with an exact running process receipt. Herdr must report the same UUID, runtime, workspace, pane, terminal, directory and foreground process group/child PID. A node checkout must still match its reserved pristine Git base. No binding intent/bootstrap or coordinator route may exist. The receipt's `stage` is `codex_before_binding`. This continues the existing process; it does not launch another Codex.

All cases require the exact epoch/generation, an eligible unexpired assignment, and an uncertain registration with no saved receiver. Existing `registration.json`, `mailer.token`, `mailer.json` or `native-receipts` entries block recovery, including partial files and dangling symlinks. Preserve that evidence: normal reconciliation consumes a valid saved receipt. On nodes, recovery verifies the original signed execute record's owner/recipient/epoch/hash and the complete normalized assignment. No record or grant is renewed.

The local launch lease excludes concurrent launch/cancellation operations. Herdr inspection runs in a read transaction so process exit receipts can still be written. Before authorization, a write transaction rechecks the exact target's launch, process and authority records, deadline, receiver files and ownership lease. A changed target refuses recovery; unrelated writes do not starve it. The authorization and its separate SQLite receipt commit together.

Use the same operation ID and epoch after an uncertain CLI response. Replays return the original authorization without resetting another registration, even if launch has progressed. A new lost response requires a new explicit operation; at most 32 authorizations are retained per target. `recorded_state` and the operation's `model_launched: false` describe this maintenance action, not current runtime health. Coordinator status exposes `registration_state` and `registration_recovery_count`.

An unlocked or missing ownership lease does not prove that the native child exited: after a wrapper crash it may still be alive. Legacy wrappers without this evidence, lost wrappers, changed native identities, already-bound receivers and malformed retained evidence remain ineligible. Capacity stays reserved until a genuine exit receipt is applied. Controller schema 9 and node schema 8 retain the recovery ledger; the ownership nonce is additional process-receipt evidence and does not rewrite old receipts. Upgrade services before using recovery; this command does not migrate or create databases. Downgrade is unsupported.

## Worker lifecycle and recovery

1. Dispatch reserves capacity and records an immutable contract/memory snapshot.
1. The node persists a worktree plan, including the exact path, branch and base commit, before asking Herdr to create it.
1. Git and Herdr observations must agree on repository, branch, checkout, workspace, pane and terminal. A reused terminal or occupied pane fails reconciliation.
1. A one-shot wrapper launches the runtime. Codex's UUID is observed on that exact pane; Claude/Pi receive a preassigned UUID.
1. The node registers the recipient with mailer, then asks the controller to bind the complete resource identity.
1. After the controller application receipt, the node sends an idempotent bootstrap message with exact mailer recipient session/epoch preconditions.
1. The model calls `worker_ready`. Only this functional response transitions the controller attempt to `running`.

A command reply, broker ACK or runtime API submission never completes steps 6–7 by itself. The worker CLI and MCP expose receipts with `queued`, `broker_stored` and `controller_applied` states.

Lost create/launch replies do not reset the effect marker. Existing resources can prove success; absence leaves uncertainty. A lost mailer **registration** response without a durable local receipt stops automatic launch. Claude/Pi before launch and still-owned Codex before receiver binding support the explicit recovery above. Send retries already use mailer operation IDs.

The pane wrapper now remains alive around the native runtime and holds a private ownership lease identified by `owner_nonce` in its process receipt. It gives the child its own process group and the foreground terminal, then restores the terminal when the child exits. It records spawn intent and a durable native-process exit receipt in the node's `runtime_processes` table. The node service forwards `observe_stopped` through its signed outbox; capacity is released only after the controller applies that event. Normal exit preserves an already-submitted result. An author that exits without a result leaves an `interrupted` task.

`stop_attempt` takes `attempt` and `expected_epoch`. It retires the worker credential and increments the epoch before emitting the stop command. Run cancellation applies the same fence to every active attempt. The wrapper observes the durable local fence, sends TERM to its owned child group and escalates to KILL after two seconds if the child remains alive. It also stops on the execution deadline without needing a live controller, or on a TERM/HUP sent to the wrapper. The controller service cancels expired runs. A received cancellation never counts as an exit receipt.

Only the wrapper signals its own unreaped child. After a wrapper crash, the node does **not** signal a stored numeric PID or infer exit from an absent pane.

### Passive recovery after wrapper loss

Dedicated wrappers reset inherited `SIGCHLD` auto-reaping before spawn so their Child handle retains the original PID until wait. On Linux, new wrappers capture `kernel_identity` while they still own their unreaped child: kernel boot UUID, PID namespace device/inode, PID and process birth ticks. Node lifecycle reconciliation and coordinator advancement can recover a missing exit receipt automatically. Recovery must acquire the existing private wrapper ownership lease, match the exact process row/epoch and observe the same boot and PID namespace. It holds that lease through a conditional SQLite update; a live wrapper retains its ordinary exit-receipt path. Invalid, missing, public, symlink or non-regular lease files cannot authorize recovery.

The observer uses `pidfd_open` and nonblocking polling. Kernel-confirmed termination, ESRCH in the recorded namespace, or a different birth identity at that PID establishes that the original native process exited. It sends no signal. A procfs mount from another PID namespace, unreadable identity, changed boot/namespace, unavailable pidfd support or a still-live original process keeps the outcome unresolved. A reboot alone is not evidence: the state might have been copied from another machine. Legacy receipts without captured identity, and a crash between spawn and its running receipt, also remain unresolved.

A recovered receipt has `reason: recovered_native_exit`, kernel `exit_evidence` and `observed_at`. Its `code` and `signal` are null; the observer cannot recover a non-child's exit status or exact exit time. It does not invent a successful task result. The node persists this receipt before its signed `observe_stopped` event, and controller application still precedes capacity release. Coordinator retirement revokes only the recovered launch generation; replay after a lost retirement receipt does not create another launch.

This backend establishes exit of the recorded native process, not termination of detached descendants or permission to delete panes/worktrees. Stopping live orphans, recovery across changed namespaces/boots, legacy capture gaps, non-Linux backends and conservative resource cleanup remain pending. The kernel contracts are documented in [pidfd_open(2)](https://man7.org/linux/man-pages/man2/pidfd_open.2.html), [proc_pid_stat(5)](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html) and [pid_namespaces(7)](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html).

The receipt's scope is `native_process`: it confirms that the owned CLI/launcher has exited, not that a remote provider stopped computing or that detached descendants have been audited. Cancellation addresses the owned process group; cleanup must separately establish that resources are no longer in use. Worktrees and Herdr panes are retained in this checkpoint.

If a reviewer exits before submitting its review, call `request_review` again with the current integration revision after its exit receipt has been applied. A live reviewer or completed review cannot be silently replaced. For an `interrupted` or `changes_requested` task, `retry_task` takes `task`, `expected_revision` and optional replacement `contract`. It requires all prior attempts to be stopped and no pending verification/apply effect, preserves immutable results and marks old candidates superseded. The next dispatch uses a new attempt, current integration base and current memory. Reviewer attempts do not consume the author's attempt budget.

`complete_run` takes `run` and `expected_revision`. It requires every task completed/integrated, no occupied process slots, no active candidate/apply intent and no earlier check or repository effect with an unresolved outcome. A successful later candidate cannot erase an older check's unknown exit. Completion releases the canonical repository binding while retaining refs and audit history.

After cancellation, `run-controller` automatically reconciles closed runs under the Git executor lease. It waits for worker exit receipts and owned coordinator retirement, refuses started checks and uncertain repository/checkout creation, and retains unresolved Git apply intents. Once the recorded effects are idle, one transaction supersedes unfinished candidates, retires their controller intents, releases the repository binding and journals the decision. Worktrees, results, checks and Git refs remain intact. Repeated cycles do not repeat the release. `repository_recovery` in snapshots/context and the Projects overview/settings explain whether release is pending, blocked (with reasons) or complete. Creating a replacement run for the same logical repository is refused until the previous cancellation is reconciled; physical aliases remain fenced by the canonical repository binding during verification.

Queued verification that never acquired a repository binding can be retired after the executor lease is available. A check finishing during cancellation retains its durable receipt and can then permit release. A Git CAS whose SQL receipt was lost must first be reconciled against the exact candidate by the existing executor; cancellation does not authorize another CAS. A preparation with uncertain side effects, a started check without an outcome, a lost native wrapper without recoverable kernel exit evidence or an unresolved owned coordinator continues to block. Explicit abandonment/recovery of those uncertain effects and deletion of retained resources remain pending; absence of a process or elapsed time is not treated as proof.

Terminal handling follows Rust's [Unix process-group API](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.process_group) and [foreground-terminal semantics](https://man7.org/linux/man-pages/man3/tcsetpgrp.3.html). A real pseudoterminal test verifies interactive input and restoration without launching a model.

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

The Git pipeline above supports `code` contracts; artifact verification and acceptance use the separate pipeline below. Abandoning an uncertain in-flight verifier and conservative worktree cleanup remain pending. Repair dispatch and reviewer replacement use the explicit lifecycle decisions above. A submitted review or integrated task does not prove that either runtime has stopped.

## Artifact content and immutable results

Ready authors can upload binary files through their scoped node API. The same private NATS protocol carries these bytes to the controller; no shared filesystem, worker-supplied URL or synthetic Git commit is required for content submission. An `artifact` contract has a complete verification/review/acceptance path. Content verification alone never marks a task complete; use the decisions below after sealing the result.

Using the worker prefix supplied at bootstrap:

```bash
herdr-supervisor control --state NODE_DIR worker \
  --credential WORKER_TOKEN --attempt ATTEMPT --epoch EPOCH \
  upload-artifact report.pdf ./report.pdf
```

The CLI reads a regular, non-symlink file into a bounded snapshot and queues 16 KiB chunks with deterministic operation IDs. Files may contain arbitrary bytes, including an empty file. Each chunk carries the filename, lowercase SHA-256 of the complete file, total byte size and zero-based index. Names are flat ASCII filenames, at most 100 characters, starting with a letter or digit; dots, underscores and hyphens are allowed after that. Paths, leading dots and directory traversal are rejected.

Limits are 8 MiB per file, 32 files and 32 MiB of declared file content per attempt. The controller reserves the declared size when the first chunk for a filename arrives, so incomplete uploads cannot evade the quota. These are logical content limits: the private signed event/inbox history and SQLite WAL also consume storage. Filenames and chunks are immutable within an attempt. Different bytes, sizes or hashes under an existing name are rejected; changing a file requires another name within the quota or a new attempt.

Repeating the CLI command with the same filename and bytes resumes the same operations and event IDs. It retries an occupied execution fence for up to a two-second retry window. Other errors, including an ambiguous IPC reply, are returned; the same command can be resumed without a new logical upload. It refuses a file that is itself a symlink, FIFO or device. An upload response reports queueing, not controller verification. Inspect the exact hash returned by the upload:

```bash
herdr-supervisor control --state NODE_DIR worker \
  --credential WORKER_TOKEN --attempt ATTEMPT --epoch EPOCH \
  artifact-status report.pdf SHA256
```

`content_verified: true` requires an authoritative controller receipt after checking the full SHA-256; `task_accepted` remains false. The status command reads authorization and receipts from one SQLite snapshot and can run while the executor holds its mutation fence. Retired/foreign worker credentials are rejected. MCP offers `worker_artifact_status` with `name` and `sha256`; `worker_apply` supports `put_artifact` with `attempt`, `epoch` and `chunk` (`name`, `sha256`, `size`, `index`, byte-array `data`). The CLI performs this chunking automatically. Reviewers cannot upload author artifacts.

After verification, an `artifact` result uses `commit: null` and an `artifacts` map such as `{"report.pdf":"<sha256>"}`. The controller rehashes stored chunks while sealing the immutable result, binds a version-1 manifest to the exact attempt/contract, and records its manifest hash. Missing chunks, unuploaded hashes or corrupted stored content prevent result submission. A result arriving before its chunks uses the durable rejected-delivery queue and can succeed later after all prerequisites arrive. Code results may also reference uploaded attachments; existing commit verification remains required.

Controller schema 7 adds `artifact_uploads` and `artifact_chunks`; node schema 7 adds owned reviewer staging while preserving existing jobs and signed events. Upload, application receipt and audit metadata commit in the ordinary controller transaction. The journal stores chunk size and SHA-256 instead of duplicating file bytes. Raw signed records and file chunks remain private SQLite state. None of these receipts proves that a model read an artifact or that an independent reviewer approved it.

## Verify, review and accept artifacts

Use the same `request_verification` decision as for code, with the author attempt and current task revision. The resulting integration has an `artifact` object containing the sealed manifest and `manifest_hash`. Its receipt identity is `artifact:sha256:<manifest_hash>`; `candidate_oid` and `tree_oid` are null and `result_oid` is empty. `base_oid` remains an actual Git commit for reviewer worktrees. Never pass the manifest hash to Git.

The controller materializes private inputs at `CONTROLLER_DIR/verification/INTEGRATION/inputs`. It persists ownership before creating files, writes and fsyncs them atomically, and runs the contract's named policy-approved check argv with this directory as the working directory. Checks can read the manifest's filenames directly. Full hashes are checked before and after checks and again before acceptance. A ready copy with changed bytes or a symlink is rejected rather than silently repaired. Lost check receipts remain uncertain and cannot trigger an automatic rerun.

When the policy requires review, `request_review` assigns a different attempt/native session. Its node fetches only that integration's declared files using signed `fetch_artifact_chunk` requests and controller receipts over NATS; shared mounts are unnecessary. Staging queues at most sixteen new requests per cycle and preserves their operation IDs through reconnect/restart. The native mailer bootstrap waits for all files to be verified, then identifies the manifest and `.herdr-supervisor-artifacts-ATTEMPT` directory inside the reviewer worktree.

The reviewer reads its original contract and `review.artifact` using worker context, then submits `submit_artifact_review` with `attempt`, `epoch`, `integration`, `manifest_hash`, `contract_hash`, `approve`, every criterion's assessment and actionable `findings`. The scoped worker API rehashes its prepared copies before queueing the review. Code-style `submit_review` and mismatched manifest/session/epoch assignments cannot approve an artifact candidate. The controller binds approval to the exact manifest and its check receipts, and queues a durable review notice for the bound native coordinator. A native send or broker receipt is not review approval.

Finally, `apply_integration` with the current integration revision queues acceptance. The controller rechecks its evidence and transactionally marks the task `completed` and integration `applied`, leaving the run's Git OID and refs unchanged. Cancellation fences this acceptance immediately, including while worker exits are pending. Task completion can satisfy dependencies, but occupied process slots remain until explicit exit receipts; stop the workers and then use `complete_run`.

Node schema 7 is required for artifact review assignments. Older nodes must be upgraded before assignment; they do not interpret artifact hashes as code candidates. Code assignments retain their existing representation. Staging files, chunks, receipts and check outputs are retained as evidence; automatic cleanup remains pending.

## Memory

Workers submit `propose_memory` with `run`, `topic`, `text`, `source_attempt` and `evidence`. The supervisor applies `approve_memory` with the proposal ID and expected topic revision. Withdrawal appends a new revision. Read via `control memory` or the authenticated API/MCP.

Memory is run-scoped and includes provenance; only requested topics plus preferences enter an assignment, bounded to 32,000 UTF-8 text bytes. Cross-run project memory and Markdown projections remain pending.

Approval/withdrawal and node delivery intents commit in one controller transaction. Each affected active attempt receives a signed `memory_update` with its exact run/attempt/epoch, a monotonically increasing run memory version and a complete selected snapshot with the same text budget. Version gaps are normal: unrelated topics do not trigger that worker's update. Nodes persist before broker ACK, reject foreign scope or content conflicts, and ignore older versions. A complete snapshot makes withdrawal durable even if updates arrive out of order. An update that precedes its execute order is durably held for local retry before TERM; it cannot be applied until the assignment exists.

`worker context` and MCP `worker_context` return both the immutable startup `memory` and `current_memory` (`version`, `snapshot`, `acknowledged_version`). The latter is the latest approved selection replicated to that node, not a promise of instantaneous consistency during a partition. Contracts and their hashes do not change. After reading the current snapshot, the worker declares that version through its existing scoped prefix:

```text
... worker --credential WORKER_TOKEN --attempt ATTEMPT --epoch EPOCH acknowledge-memory VERSION
```

MCP exposes the same operation as `worker_acknowledge_memory`. A stale version or foreign/retired credential fails. `acknowledged_version` is null until an explicit acknowledgement, including for initial version 0. The acknowledgement is stored in the node database; it is an agent declaration, not proof of its subsequent behavior or a controller decision.

Once the native bootstrap has completed, the node sends a hint through mailer to the frozen recipient session/epoch. The hint contains context instructions, not memory text or a new task. Delivery has its own durable intent, stable operation ID, exact recipient preconditions and validated receipt. Replays retain the frozen body and sender-token fingerprint; a changed token blocks that retry instead of silently changing origin. Preserve the same mailer state/deduplication database. Mailer must advertise protocol-2 send idempotency and recipient preconditions. Cancellation, epoch retirement and the assignment deadline retire pending hints. The service attempts one hint per cycle and rotates failures using persisted attempt counts. Delivery alone never updates the worker's acknowledgement.

Node schema 4 migrates the existing assignment snapshots and preserves jobs/process receipts/events. Assignments written before `memory_version` existed retain version 0; subsequent approved updates advance them. Existing snapshots are not silently reconstructed from a newer controller history during migration. Nodes must support `memory_update` to consume these events; older nodes reject and leave them pending. This path has real-broker/process tests with simulated model/Herdr/mailer boundaries; it does not prove a live model has read or followed the memory.

## Native Herdr change hints

Continuous nodes and the controller's coordinator launcher subscribe to Herdr on separate Unix connections. Endpoints come from explicit executor configuration and saved launch/resource plans. Subscriptions cover resource lifecycle events and agent status for known panes; terminal output is not subscribed. After acknowledgement, each connection reads a fresh `session.snapshot` on a second connection while draining events. EOF, malformed/oversized frames, subscription errors (including `events_lost`) or snapshot failures trigger reconnect with backoff from one to thirty seconds and a new snapshot. The inspected Herdr 0.9.1 bundled schema reports protocol 22; older snapshot protocols are rejected by this optional hints path.

Events and snapshots only wake the existing reconciliation handlers. They never establish readiness, exit, released capacity, task completion or model consumption. The existing 500 ms idle reconciliation remains available during outages or failed subscription-target discovery; actual cycle time also includes bounded relay/effect work. A single coalesced hint and a 100 ms minimum interval limit wakeups during bursts. Partial frames retain their bytes and original three-second deadline across snapshot completion. Event frames are limited to 256 KiB and snapshots to 4 MiB.

If a named pane is no longer subscribable, a new lifecycle-only subscription preserves general hints; exact pane checks continue through periodic reconciliation. Changing the saved target set or reconnecting retries the pane subscriptions. At most 64 sockets and 256 named panes per socket are watched; excess targets still use periodic reconciliation. Node `run-node --once` output includes `herdr_events` counters (`watching`, `lifecycle_only`, `polling_only_sockets`, `polling_only_panes`); a one-shot invocation can finish before the asynchronous handshake. Service shutdown cancels owned subscriptions without changing resource/task state.

This follows the [Herdr socket API](https://herdr.dev/docs/socket-api/) distinction between live event hints and authoritative snapshots. Protocol tests use private Unix sockets and the inspected schema; they do not establish live Herdr or authenticated model acceptance.

## Backups

Use SQLite's online backup mechanism or stop the owning processes before copying a complete state directory. Copying only a live `.sqlite` file can omit WAL transactions. Back up owner/node keys and credentials privately with their corresponding database; do not rotate them merely to recover a process crash.
