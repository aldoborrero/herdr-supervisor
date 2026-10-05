{ flake, ... }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.herdr-supervisor;
  shared = import ../../support/service-units.nix { inherit lib pkgs cfg; };
in
{
  options.services.herdr-supervisor =
    (import ../../support/service-options.nix {
      inherit lib;
      defaultStateDirectory = "/var/lib/herdr-supervisor";
      defaultPackage = flake.packages.${pkgs.stdenv.hostPlatform.system}.herdr-supervisor;
    })
    // {
      user = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Existing non-root Unix user that owns the Herdr session, mailer, repositories and initialized supervisor state.";
      };
    };
  config = lib.mkIf shared.enabled {
    assertions = shared.assertions ++ [
      {
        assertion =
          cfg.user != null
          && cfg.user != "root"
          && builtins.hasAttr cfg.user config.users.users
          && config.users.users.${cfg.user}.uid != 0;
        message = "services.herdr-supervisor.user must name an existing non-root runtime owner.";
      }
    ];
    environment.systemPackages = [ cfg.package ];
    systemd.services = lib.listToAttrs (
      map (
        unit:
        lib.nameValuePair "herdr-supervisor-${unit.name}" {
          inherit (unit) description requires;
          wantedBy = [ "multi-user.target" ];
          wants = [ "network-online.target" ];
          after = [ "network-online.target" ] ++ unit.after ++ unit.requires;
          path = cfg.extraPackages;
          startLimitIntervalSec = 60;
          startLimitBurst = 5;
          serviceConfig = unit.serviceConfig // {
            User = cfg.user;
          };
        }
      ) shared.units
    );
  };
}
