# Create your first supervised project

This guide creates a controller run and its Projects view together. Creation starts no models. The controller stores the objective and policy in SQLite; the view reads that authority through its private API.

## Prepare the services

Build the local checkout with `nix build .#herdr-supervisor`; use `./result/bin/herdr-supervisor` below if the binary is not on your PATH. This does not install the plugin or activate services.

The controller must already be initialized and its API running. For a creation-only trial, the README's isolated `control init` and `control serve` commands suffice. Executing tasks also needs:

- A continuous controller with its repository mappings and notifier configuration.
- An enrolled execution node with its repository mappings and native runtime configuration.
- Private NATS JetStream, Herdr, protocol-2 herdr-mailer and an authenticated Codex, Claude or Pi runtime.

Follow [operations](operations.md) for enrollment, broker permissions and runtime setup, or the opt-in [NixOS/Home Manager service guide](services.md). The creation form does not provision these services. Keep credentials in private runtime files outside repositories and the Nix store.

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
