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

Press **N** in the project list or picker (outside its search field). Enter the name, objective, controller directory, repository identity and full base commit. An empty credential field uses `admin.token` in the controller directory. Repository identity is the absolute logical path configured in the controller and node maps; the base must be a real commit available to those repositories.

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

## Start the coordinator when ready

If you supplied a launcher configuration and enabled the controller notifier, use:

```bash
herdr-supervisor --root PROJECTS_DIR open demo
```

With several policy profiles, select one using `--profile NAME`. The popup's **c** action uses the same managed launch path. Alternatively, [attach an already registered coordinator](operations.md#connect-a-native-coordinator). Native delivery, readiness, review and completion have separate receipts; a submitted message does not prove model processing.

For an existing empty Projects folder and separately created run, [managed attach](operations.md#attach-the-projects-view) remains available. It does not migrate an active legacy project. The [original getting-started guide](upstream/getting-started.md) is retained for attribution and historical reference, not as the managed setup procedure.
