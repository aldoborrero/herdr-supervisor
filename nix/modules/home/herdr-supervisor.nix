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
  options.services.herdr-supervisor = import ../../support/service-options.nix {
    inherit lib;
    defaultStateDirectory = "${config.xdg.stateHome}/herdr-supervisor";
    defaultPackage = flake.packages.${pkgs.stdenv.hostPlatform.system}.herdr-supervisor;
  };
  config = lib.mkIf shared.enabled {
    assertions = shared.assertions ++ [
      {
        assertion = config.home.username != "root";
        message = "Herdr supervisor user services require a non-root runtime owner.";
      }
    ];
    home.packages = [ cfg.package ];
    systemd.user.services = lib.listToAttrs (
      map (
        unit:
        lib.nameValuePair "herdr-supervisor-${unit.name}" {
          Unit = {
            Description = unit.description;
            After = unit.after ++ unit.requires;
            Requires = unit.requires;
            StartLimitIntervalSec = 60;
            StartLimitBurst = 5;
          };
          Service = unit.serviceConfig // {
            Environment = [
              "PATH=${
                lib.makeBinPath (
                  cfg.extraPackages
                  ++ [
                    cfg.package
                    pkgs.coreutils
                    pkgs.git
                    pkgs.openssh
                  ]
                )
              }"
            ];
          };
          Install.WantedBy = [ "default.target" ];
        }
      ) shared.units
    );
  };
}
