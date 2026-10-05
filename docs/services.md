# NixOS and Home Manager services

The flake exports `nixosModules.herdr-supervisor` and `homeModules.herdr-supervisor`. Both are opt-in Linux/systemd modules. Importing a module starts nothing: enable a controller, one or more named nodes, or both. Home Manager runs user units; NixOS runs system units as an explicitly selected existing non-root user.

Use the same Unix owner as Herdr, mailer and the repositories. Choose one systemd manager for a state directory. The controller and each node require distinct, normalized absolute state paths. Enabled node instances cannot duplicate the same owner DID/node ID pair. Native processes launched inside Herdr remain owned by their pane wrappers; stopping a controller/node service is **not** a request to cancel its run or stop all workers. Use the controller lifecycle API for cancellation and inspect durable exit receipts.

## Prepare runtime state

Before enabling a service, follow [operations](operations.md) to initialize the controller, create/enroll a node key, transfer its signed grant and node credential, provision the private NATS consumers, and configure repository access. For example, as the runtime user:

```bash
herdr-supervisor control --state /home/aldo/.local/state/herdr-supervisor/controller init
herdr-supervisor control --state /home/aldo/.local/state/herdr-supervisor/nodes/local node-key
```

Initialization does not enroll the node or provision NATS. Those require the owner-authorized steps in the operations guide. Services deliberately do not create replacement identities on startup. Keep directories 0700 and credential/key files 0600. The NixOS default state root is `/var/lib/herdr-supervisor`; provision ownership there first, or select paths under the runtime user's home as below.

For a same-host controller/node, [the resumable local bundle command](getting-started.md#prepare-a-local-service-bundle) performs initialization, enrollment and configuration together, with optional explicit NATS provisioning. Point these module options at its output files. Generating a bundle does not activate systemd units or establish native runtime health.

The module options name existing JSON configuration files. They do not read those files during Nix evaluation and expose no option for raw tokens, broker passwords or private keys. JSON may be generated declaratively when it contains only public settings and **paths** to private runtime credentials. Never interpolate secret contents with `builtins.readFile`, include passwords in URLs or put private keys in the Nix store. The Rust services perform runtime credential and grant validation.

## NixOS

Add this flake as an input in your own configuration. The module defaults to the package from its publishing flake; `package` can override it explicitly.

```nix
{ inputs, pkgs, ... }:
{
  imports = [ inputs.herdr-supervisor.nixosModules.herdr-supervisor ];

  services.herdr-supervisor = {
    user = "aldo"; # Existing account that owns Herdr and mailer.
    extraPackages = [ pkgs.cargo ]; # Tools needed by controller checks.
    controller = {
      enable = true;
      stateDirectory = "/home/aldo/.local/state/herdr-supervisor/controller";
      brokerConfigFile = "/home/aldo/.config/herdr-supervisor/controller-broker.json";
      repositoriesConfigFile = "/home/aldo/.config/herdr-supervisor/repositories.json";
      coordinatorConfigFile = "/home/aldo/.config/herdr-supervisor/notifier.json";
    };
    nodes.local = {
      enable = true;
      stateDirectory = "/home/aldo/.local/state/herdr-supervisor/nodes/local";
      ownerDid = "did:key:z6MkREPLACE_WITH_INITIALIZED_OWNER";
      grantFile = "/home/aldo/.config/herdr-supervisor/local-grant.json";
      brokerConfigFile = "/home/aldo/.config/herdr-supervisor/node-broker.json";
      executorConfigFile = "/home/aldo/.config/herdr-supervisor/executor.json";
      runtimeConfigFile = "/home/aldo/.config/herdr-supervisor/runtime.json";
    };
  };
}
```

Replace the example DID with the controller's actual public owner DID. `nodes.local.nodeId` defaults to `local`; another attribute name creates another independently configured unit. A remote execution host can import the same module with only its node enabled. The controller owner key stays on the controller host.

## Home Manager

The user module uses the same instance options, with state below `${config.xdg.stateHome}/herdr-supervisor`. It does not take a `user` option: the home owner runs the units.

```nix
{ inputs, ... }:
{
  imports = [ inputs.herdr-supervisor.homeModules.herdr-supervisor ];

  services.herdr-supervisor.controller = {
    enable = true;
    brokerConfigFile = "/home/aldo/.config/herdr-supervisor/controller-broker.json";
    repositoriesConfigFile = "/home/aldo/.config/herdr-supervisor/repositories.json";
    coordinatorConfigFile = "/home/aldo/.config/herdr-supervisor/notifier.json";
  };
}
```

Add `services.herdr-supervisor.nodes.local` with the node settings from the NixOS example when this user also runs an execution node. User services are wanted by `default.target`; persistence after logout depends on the host's existing user-manager/linger policy. The module does not change that host policy.

## Dependencies and operations

A controller unit is named `herdr-supervisor-controller.service`; node units are `herdr-supervisor-node-<name>.service`. Each instance has `after` and `requires` lists for existing units in the **same** systemd manager. For example, `requires = [ "herdr-mailer.service" ];` also orders the supervisor after that unit. A system unit cannot express a dependency on a user-manager unit through these options. Herdr, mailer, NetBird and NATS remain separately configured services.

`coordinatorConfigFile` enables the notifier and owned coordinator launcher pump in `run-controller`. It contains a registered mailer sender credential **path**, not the mailer administrator token. Automatic launch from Projects additionally requires the owner-approved launch configuration supplied to `managed attach --launch-config`; see [coordinator launch](operations.md#launch-a-coordinator-from-projects).

`extraPackages` adds tools to the service PATH, especially for controller checks. Native binaries are selected by absolute paths in executor/runtime/launch JSON. A process launched by Herdr inherits its pane environment, so adding a service PATH entry does not install tools into every native agent's environment.

Services use UMask 0077, retry failures every 15 seconds without exhausting the normal start-rate limit during a prolonged broker outage, and send SIGINT to the main service process on stop. `KillMode=mixed` leaves in-flight checks time to produce their receipts; `stopTimeout` defaults to `"infinity"` so systemd does not interrupt that drain. An explicit finite override may force a kill and leave a durable intent uncertain. Restart reconciles existing intents instead of deleting state. Run deadlines and cancellation still govern worker/native process lifetime independently.

After your own activation, inspect units with `systemctl status herdr-supervisor-controller` or `systemctl --user status herdr-supervisor-node-local`, and corresponding `journalctl -u` / `journalctl --user -u` commands. These modules and their checks do not activate anything in the development host.

## Evidence and limits

`checks.<system>.service-modules` evaluates the exported modules against the flake's pinned NixOS and Home Manager, including disabled imports, owner identity, required files, multiple nodes, state separation, command arguments, explicit dependencies, stop behavior and systemd escaping. It also checks rendered NixOS/Home Manager unit files. Darwin evaluation checks that an enabled user service requires Linux. Tests use absent fixture configuration paths, proving that evaluating a module does not read runtime secret files.

These are evaluation/rendering checks, not evidence of a live systemd deployment, live Herdr session or authenticated model. The existing Rust suite covers the service loops and isolated native process behavior. Full continuous-node workflow coverage, grant renewal, orphan recovery and conservative cleanup remain tracked in [IMPLEMENTATION.md](../IMPLEMENTATION.md).
