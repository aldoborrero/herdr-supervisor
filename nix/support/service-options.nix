{
  lib,
  defaultStateDirectory,
  defaultPackage,
}:
let
  inherit (lib) mkOption mkEnableOption types;
  pathFile =
    description:
    mkOption {
      type = types.nullOr types.str;
      default = null;
      inherit description;
    };
  common = state: {
    enable = mkEnableOption "this Herdr supervisor service";
    stateDirectory = mkOption {
      type = types.str;
      default = state;
      description = "Absolute normalized private state directory; initialize it as the runtime owner before enabling the service.";
    };
    brokerConfigFile = pathFile "Absolute path to broker JSON. Its credentials field names a private runtime file; never put credential contents in Nix.";
    after = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Additional units to order after, such as the existing Herdr or mailer service. Ordering does not start those units.";
    };
    requires = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Existing services required by this instance, if managed by the same systemd manager.";
    };
  };
in
{
  package = mkOption {
    type = types.package;
    default = defaultPackage;
    description = "Herdr supervisor package; defaults to this module's publishing flake.";
  };
  extraPackages = mkOption {
    type = types.listOf types.package;
    default = [ ];
    description = "Tools on the service PATH, for example Cargo for controller checks. Native runtime binaries remain configured by absolute path.";
  };
  stopTimeout = mkOption {
    type = types.str;
    default = "infinity";
    description = "systemd stop timeout. The default allows owned checks and launch effects to drain after SIGINT; forcing a timeout can leave an uncertain intent.";
  };
  controller = mkOption {
    default = { };
    type = types.submodule {
      options = common "${defaultStateDirectory}/controller" // {
        repositoriesConfigFile = pathFile "Absolute path to the controller repository/check configuration JSON.";
        coordinatorConfigFile = pathFile "Optional absolute path to native coordinator notifier JSON; also enables the owned coordinator launch pump.";
      };
    };
    description = "Controller API, NATS relay, Git verifier and optional native coordinator services.";
  };
  nodes = mkOption {
    default = { };
    type = types.attrsOf (
      types.submodule (
        { name, ... }: {
          options = common "${defaultStateDirectory}/nodes/${name}" // {
            nodeId = mkOption {
              type = types.str;
              default = name;
              description = "Node ID enrolled at the controller; distinct from this unit's instance name.";
            };
            ownerDid = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = "Pinned controller owner did:key; public identity, not an owner private key.";
            };
            grantFile = pathFile "Absolute path to the owner-signed node delegation. Renewal remains an explicit operator action.";
            executorConfigFile = pathFile "Absolute path to node repository/worktree/Herdr executor JSON.";
            runtimeConfigFile = pathFile "Absolute path to runtime/mailer configuration JSON, containing credential paths rather than secret values.";
          };
        }
      )
    );
    description = "Independently enrolled execution nodes. Each requires its own state, node key, grant and broker configuration.";
  };
}
