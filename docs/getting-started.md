# Create your first supervised project

This guide creates a controller run and its Projects view together. Creation starts no models. The controller stores the objective and policy in SQLite; the view reads that authority through its private API.

## Prepare the services

Build the local checkout with `nix build .#herdr-supervisor`; use `./result/bin/herdr-supervisor` below if the binary is not on your PATH. This does not install the plugin or activate services.

The controller must already be initialized and its API running. For a creation-only trial, the README's isolated `control init` and `control serve` commands suffice. Executing tasks also needs:

- A continuous controller with its repository mappings and notifier configuration.
- An enrolled execution node with its repository mappings and native runtime configuration.
- Private NATS JetStream, Herdr, protocol-2 herdr-mailer and an authenticated Codex, Claude or Pi runtime.

Follow [operations](operations.md) for enrollment, broker permissions and runtime setup, or the opt-in [NixOS/Home Manager service guide](services.md). The creation form does not provision these services. Keep credentials in private runtime files outside repositories and the Nix store.

### Prepare a local service bundle

For a controller and execution node on the same machine, `control setup-local` combines initialization, node enrollment, the signed grant and service configuration. It uses the same NATS node protocol as remote execution. Supply one JSON specification, for example:

```json
{
  "node": "local",
  "node_state": "/private/supervisor-node",
  "max_active": 3,
  "grant_expires_at": 1900000000,
  "repositories": { "/repos/demo": "/home/aldo/Dev/demo" },
  "worktree_root": "/private/supervisor-worktrees",
  "check_timeout_secs": 300,
  "broker": {
    "url": "nats://127.0.0.1:4222",
    "admin_credentials": "/private/nats-admin.creds",
    "controller_credentials": "/private/nats-controller.creds",
    "node_credentials": "/private/nats-local.creds",
    "private_network_plaintext": true
  },
  "native": {
    "herdr_binary": "/absolute/bin/herdr",
    "herdr_socket": "/private/herdr.sock",
    "supervisor_binary": "/absolute/bin/herdr-supervisor",
    "mailer_state": "/private/mailer",
    "mailer_admin_credential": "/private/mailer-admin.token",
    "codex_binary": "/absolute/bin/codex",
    "mailer_runtime_binary": "/absolute/bin/herdr-mailer-runtime",
    "claude_development_channel": false
  },
  "notifier_credential": "/private/supervisor-notifier.token"
}
```

Replace the illustrative paths and expiry before use. `grant_expires_at` is an explicit Unix timestamp; `date -d '+7 days' +%s` can calculate one on NixOS. Claude requires an explicit development-channel opt-in when you choose that runtime. The notifier credential is a registered mailer sender token, distinct from the mailer admin credential. Broker credentials name three separate runtime files/accounts: administrator, controller and node. The command does not generate those external service credentials. Omit `private_network_plaintext` or set it to false for required TLS; the example opts into plaintext only for a private local broker.

```bash
herdr-supervisor control --state /private/supervisor-controller setup-local \
  --spec local-services.json --bundle /private/supervisor-services
```

This creates private controller/node databases and keys, enrolls one node through the owner mutation API and writes the bundle with mode-0600 files. The owner key stays in the controller directory. The bundle contains `node.token`, `node-grant.json`, separate broker configurations, repository/executor/runtime/notifier/launcher JSON, public broker permissions and `commands.json`. The JSON result supplies exact argv arrays for controller, node and broker provisioning; paths with spaces remain single arguments. It reports `services_started: false`, `models_started: false` and `runtime_health_checked: false`.

The command preserves identities and enrollment on repeat. Its frozen `intent.json` and `authority.json` retain the operation, specification and authority fingerprints. Reuse the same specification and bundle after an interrupted publication or failed broker connection. Existing output files must match; an edited file, changed authority, retired node or expired grant stops the retry. No grant renewal or credential rotation is inferred. Controller, node, bundle, mailer and worktree roots must be distinct and non-overlapping, with service state outside mapped repositories; aliases through existing symlinks are checked before initialization.

Apply the generated `broker-policy.json` to distinct accounts on your private NATS server. This export covers the configured local node; for a controller with additional nodes, generate its complete permissions with `control broker-policy` instead of replacing them with the local-only export. Once those credentials and the server are ready, add `--provision` to the same command to create or validate its stream and controller/node consumers. Only this explicit flag contacts NATS, using the administrator credential. A failed provision leaves the local bundle available for retry. `broker_provisioned_this_invocation` means that request succeeded now; an offline repeat reports false instead of reusing an old health claim.

Start the services with the returned argv, or reference these bundle paths in the opt-in NixOS/Home Manager modules. The tool does not install units, start Herdr/mailer/NATS, authenticate models or change global configuration. Use the bundle's `launch.json` when creating the project below. A multi-host deployment still follows the explicit key/grant transfer and node mappings in [operations](operations.md#enroll-a-node-and-provision-nats); never copy the controller owner key to a remote node.

### Check the bundle before starting

After preparing the bundle and its external services, run:

```bash
herdr-supervisor control --state /private/supervisor-controller check-local \
  --bundle /private/supervisor-services
```

The command prints a JSON report and returns nonzero when any check fails. `bundle_integrity` checks the frozen configuration, file privacy, owner/node keys and capabilities, original enrollment receipt, both database authorities and signed grant. A failed integrity check stops before contacting external services. It opens the databases read-only and never initializes missing state, repairs files, renews grants or migrates a schema.

The remaining checks use the controller and node's runtime broker credentials to read their consumers, read Herdr's protocol-22-or-later snapshot, authenticate the notifier with mailer's protocol-2 capabilities/doctor, check that the private mailer admin file is readable, and inspect mapped repositories with Git. The broker administrator credential is not used. A final authority read detects changes during those observations. External error bodies are not copied into the report; each failed component has an action explaining what to inspect.

`checks_passed` is a set of current observations, not an atomic guarantee that later startup will succeed. `model_readiness` is explicitly `not_checked`, including when every dependency check passes. `not_checked` lists model authentication, native CLI compatibility, mailer admin authorization, broker ACL isolation, repository write access and worker readiness. In particular, reading the admin file does not prove that the daemon accepts it; mailer has no read-only admin-authentication request. No runtime is launched, registered or messaged. Keep the normal exact-session bootstrap and `worker_ready` handshake when starting work.

## Create from the popup

```bash
herdr-supervisor --root PROJECTS_DIR popup
```

The plugin menu action **Supervisor: new managed project** opens this form directly. From an existing popup, press **N** in the project list or picker (outside its search field). Enter the name, objective, controller directory, repository identity and full base commit. An empty credential field uses `admin.token` in the controller directory. Repository identity is the absolute logical path configured in the controller and node maps; the base must be a real commit available to those repositories.

Use Tab/Up/Down to navigate and Left/Right to choose Codex, Claude or Pi. The form creates a single profile named `auto`, reserves one agent slot for independent review and requires review. Set the active-agent limit, task-attempt limit and run duration. An optional model becomes a literal `--model` argument. Enter a check as JSON argv, for example `["cargo", "test", "--locked"]`; it is named `test`. `[]` configures no check. Shell expansion is not applied to this argv.

The optional launcher configuration enables a later coordinator launch; see [launcher setup](operations.md#launch-a-coordinator-from-projects). Enter submits the form and Escape closes it. Validation or controller errors retain your input. Success selects the new managed project; it does not dispatch a worker or start a coordinator.

## Create from a specification

For multiple profiles or checks, save this shape as `demo.json`. Replace all paths and the illustrative commit ID with your actual configuration:

```json
{
  "name": "Demo",
  "controller": "/private/controller",
  "credential": "/private/controller/admin.token",
  "repo": "/repos/demo",
  "base_oid": "0123456789012345678901234567890123456789",
  "objective": "Implement the agreed parser changes",
  "policy": {
    "max_active": 3,
    "reserved_reviewers": 1,
    "max_attempts": 3,
    "duration_secs": 3600,
    "require_review": true,
    "profiles": {
      "implementation": { "runtime": "codex", "argv": [], "limit": 2 }
    },
    "checks": { "tests": ["cargo", "test", "--locked"] }
  },
  "launch_config": null
}
```

```bash
herdr-supervisor --root PROJECTS_DIR managed create --spec demo.json
herdr-supervisor --root PROJECTS_DIR managed status demo
herdr-supervisor --root PROJECTS_DIR popup demo
```

The receipt identifies the project, run, operation and published path, with `models_started: false`. Read-only specification files may be Nix-managed symlinks. Their credential field contains a file path, never a token. `--spec-json` accepts the same document directly as one argument.

Creation validates policy and receipt scope; repository availability is checked by the execution/verification paths later. A successful create is not a health check of every service or a successful model launch.

## Recover an interrupted creation

```bash
herdr-supervisor --root PROJECTS_DIR managed setup-status demo
herdr-supervisor --root PROJECTS_DIR managed resume-setup demo
```

In the form, enter the saved project name and press **Ctrl-R** to resume with the saved specification; the other fields are ignored. Repeating `managed create` with identical inputs is also safe. The saved operation and credential fingerprint remain fixed, including when the controller applied the request but its response was lost.

`setup-status` reads the local creation receipt. Its `recorded_state` is `controller_unconfirmed`, `publication_unconfirmed` or `published`; it explicitly does not verify current controller state. Use `managed status` after publication for a fresh controller read.

Creation builds an already-bound view privately, then publishes it atomically without replacing an existing directory or symlink. A conflicting destination, changed credential or changed specification stops recovery. Existing operator edits survive a replay. A previously published folder that was subsequently removed is not silently recreated. Hidden `.managed-setup/<slug>/` intents and interrupted staging folders are retained for diagnosis; automatic cleanup is not implemented. Do not delete this state to retry an uncertain request.

## Add a task from the popup

Open the **Tasks** section and select **Create a task…** (Enter or `n`). This row appears for an active run with available controller context. Fill in the title, instructions, acceptance criteria and result type (`code` or `artifact`). Criteria are a JSON list of strings. Checks default to all checks in the captured run policy; memory topics and predecessor task IDs are also JSON lists. Only checks authorized by that policy and dependencies from that run are accepted.

Enter creates the task through `managed apply`. It does not dispatch an agent. The request retains the run revision captured when the form opened, so a concurrent change may require returning to the refreshed task list and opening a new form. Controller errors keep the submitted form visible. Once submitted, its request is frozen: Enter retries exactly the same operation and Escape returns to inspect current state. Do not interpret Escape as cancelling a request that the controller may already have accepted. After an uncertain response, inspect the task list before intentionally creating another task.

The form writes its JSON request to CLI stdin, so large instructions do not become one oversized process argument. Select a **planned** task and press **e** to edit its contract and complete dependency list. The form preserves its existing fields and captures that task's revision. Saving uses the same frozen-retry behavior as creation. Run profiles can be managed from Settings as described below.

## Choose profiles for future agents

With an owner credential, open **Settings** and select **Create a run profile…**. Choose Codex, Claude or Pi, enter model arguments as a JSON list (for example `["--model","MODEL"]`) and set the maximum active attempts for that name. Press **e** on a profile to edit it, or **d** to confirm removal. You must retain at least one profile. Supervisor credentials can inspect profiles but cannot edit them.

Changes affect the next reservations; existing agents keep their assigned settings. Reducing a limit does not terminate agents. The form retains the run revision and freezes submitted requests for exact retries, just like task entry. See [profile operations](operations.md#edit-run-profiles) for API, migration and verification behavior.

## Start the coordinator when ready

If you supplied a launcher configuration and enabled the controller notifier, use:

```bash
herdr-supervisor --root PROJECTS_DIR open demo
```

With several policy profiles, select one using `--profile NAME`. The popup's **c** action uses the same managed launch path. Alternatively, [attach an already registered coordinator](operations.md#connect-a-native-coordinator). Native delivery, readiness, review and completion have separate receipts; a submitted message does not prove model processing.

For an existing empty Projects folder and separately created run, [managed attach](operations.md#attach-the-projects-view) remains available. It does not migrate an active legacy project. The [original getting-started guide](upstream/getting-started.md) is retained for attribution and historical reference, not as the managed setup procedure.
