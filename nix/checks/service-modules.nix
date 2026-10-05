{
  flake,
  inputs,
  pkgs,
  ...
}:
let
  inherit (pkgs) lib;
  package = pkgs.writeShellApplication {
    name = "herdr-supervisor";
    text = ''printf '%s\n' "$@"'';
  };
  controller = {
    enable = true;
    brokerConfigFile = "/private/controller-broker.json";
    repositoriesConfigFile = "/private/repositories.json";
    coordinatorConfigFile = "/private/notifier.json";
    requires = [ "herdr-mailer.service" ];
    after = [ "herdr.service" ];
  };
  node = {
    enable = true;
    ownerDid = "did:key:z6MkExamplePinnedOwner";
    brokerConfigFile = "/private/node-broker.json";
    grantFile = "/private/delegation.json";
    executorConfigFile = "/private/executor.json";
    runtimeConfigFile = "/private/runtime.json";
  };
  settings = {
    inherit package controller;
    nodes.local = node;
  };
  nixos =
    cfg:
    (import (inputs.nixpkgs + "/nixos/lib/eval-config.nix") {
      inherit pkgs;
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        flake.nixosModules.herdr-supervisor
        {
          system.stateVersion = "26.05";
          boot.isContainer = true;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          users.users.operator = {
            isNormalUser = true;
            home = "/home/operator";
          };
          services.herdr-supervisor = cfg;
        }
      ];
    }).config;
  home =
    cfg:
    (inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        flake.homeModules.herdr-supervisor
        {
          home = {
            username = "operator";
            homeDirectory = "/home/operator";
            stateVersion = "26.05";
          };
          services.herdr-supervisor = cfg;
        }
      ];
    }).config;
  system = nixos (settings // { user = "operator"; });
  user = home settings;
  s = system.systemd.services.herdr-supervisor-controller;
  n = system.systemd.services.herdr-supervisor-node-local;
  h = user.systemd.user.services.herdr-supervisor-controller;
  hn = user.systemd.user.services.herdr-supervisor-node-local;
  failures = cfg: map (a: a.message) (lib.filter (a: !a.assertion) cfg.assertions);
  valid = cfg: failures cfg == [ ];
  names = units: lib.filter (lib.hasPrefix "herdr-supervisor-") (builtins.attrNames units);
  rejected =
    cfg:
    !valid (
      nixos (
        {
          inherit package;
          user = "operator";
        }
        // cfg
      )
    );
  strange = home {
    inherit package;
    controller = controller // {
      stateDirectory = ''/private/run $TOKEN %h "quoted"'';
    };
  };
  exec =
    service:
    if builtins.isList service.ExecStart then lib.head service.ExecStart else service.ExecStart;
  strangeStart = exec strange.systemd.user.services.herdr-supervisor-controller.Service;
  two = home {
    inherit package;
    nodes = {
      local = node;
      remote = node // {
        nodeId = "runner-2";
      };
    };
  };
  report =
    if !pkgs.stdenv.hostPlatform.isLinux then
      {
        disabledHomeHasNoUnits = names (home { }).systemd.user.services == [ ];
        enabledHomeRequiresLinux =
          !(builtins.tryEval (builtins.deepSeq (home settings).systemd.user.services true)).success;
      }
    else
      {
        nixosExportExists = flake ? nixosModules.herdr-supervisor;
        homeExportExists = flake ? homeModules.herdr-supervisor;
        disabledSystemHasNoUnits = names (nixos { }).systemd.services == [ ];
        disabledHomeHasNoUnits = names (home { }).systemd.user.services == [ ];
        enabledSystemAssertions = valid system;
        enabledHomeAssertions = valid user;
        controllerAndNodeUnits =
          names system.systemd.services == [
            "herdr-supervisor-controller"
            "herdr-supervisor-node-local"
          ];
        systemRunsAsRuntimeOwner = s.serviceConfig.User == "operator" && n.serviceConfig.User == "operator";
        homeUsesUserManager = !(h.Service ? User) && h.Install.WantedBy == [ "default.target" ];
        systemStartupTarget = s.wantedBy == [ "multi-user.target" ];
        shutdownPreservesInFlightWork =
          s.serviceConfig.KillSignal == "SIGINT"
          && s.serviceConfig.KillMode == "mixed"
          && s.serviceConfig.TimeoutStopSec == "infinity"
          && h.Service.KillSignal == "SIGINT"
          && hn.Service.KillMode == "mixed";
        outageRetriesStayBelowStartLimit =
          s.serviceConfig.RestartSec * s.startLimitBurst >= s.startLimitIntervalSec
          && h.Service.RestartSec * h.Unit.StartLimitBurst >= h.Unit.StartLimitIntervalSec;
        privateFilesByDefault = s.serviceConfig.UMask == "0077" && hn.Service.UMask == "0077";
        restartIsBounded =
          s.serviceConfig.Restart == "on-failure"
          && s.startLimitBurst == 5
          && h.Unit.StartLimitIntervalSec == 60;
        explicitDependencies =
          s.requires == [ "herdr-mailer.service" ]
          && builtins.elem "herdr-mailer.service" s.after
          &&
            h.Unit.After == [
              "herdr.service"
              "herdr-mailer.service"
            ];
        correctControllerCommand = lib.hasInfix ''"run-controller" "--broker" "/private/controller-broker.json" "--repositories" "/private/repositories.json" "--coordinator-config" "/private/notifier.json"'' s.serviceConfig.ExecStart;
        correctNodeCommand =
          lib.hasInfix ''"run-node" "--node" "local" "--owner" "did:key:z6MkExamplePinnedOwner" "--grant" "/private/delegation.json"'' (
            exec hn.Service
          )
          && lib.hasInfix ''"--executor" "/private/executor.json" "--runtime-config" "/private/runtime.json"'' (
            exec hn.Service
          );
        defaultsAreSeparate =
          lib.hasInfix ''"/var/lib/herdr-supervisor/controller"'' s.serviceConfig.ExecStart
          && lib.hasInfix ''"/var/lib/herdr-supervisor/nodes/local"'' n.serviceConfig.ExecStart
          && lib.hasInfix ''"/home/operator/.local/state/herdr-supervisor/controller"'' (exec h.Service);
        noAutomaticIdentityInitialization =
          !(h.Service ? ExecStartPre) && !(s.serviceConfig ? ExecStartPre);
        multipleNodes =
          names two.systemd.user.services == [
            "herdr-supervisor-node-local"
            "herdr-supervisor-node-remote"
          ]
          && lib.hasInfix ''"--node" "runner-2"'' (
            exec two.systemd.user.services.herdr-supervisor-node-remote.Service
          );
        systemdEscaping =
          lib.hasInfix ''/private/run $$TOKEN %%h \"quoted\"'' strangeStart
          && !(lib.hasInfix "/bin/sh" strangeStart);
        missingBrokerRefused = rejected {
          controller = controller // {
            brokerConfigFile = null;
          };
        };
        missingNodeGrantRefused = rejected {
          nodes.local = node // {
            grantFile = null;
          };
        };
        rootUserRefused = !valid (nixos (settings // { user = "root"; }));
        unknownUserRefused = !valid (nixos (settings // { user = "missing"; }));
        sharedStateRefused = rejected {
          inherit controller;
          nodes.local = node // {
            stateDirectory = "/var/lib/herdr-supervisor/controller";
          };
        };
        relativeStateRefused = rejected {
          controller = controller // {
            stateDirectory = "state";
          };
        };
        nonNormalizedStateRefused = rejected {
          controller = controller // {
            stateDirectory = "/private/a/../b";
          };
        };
        storeStateRefused = rejected {
          controller = controller // {
            stateDirectory = "/nix/store/state";
          };
        };
        relativeConfigRefused = rejected {
          controller = controller // {
            repositoriesConfigFile = "repositories.json";
          };
        };
        invalidNodeIdRefused = rejected {
          nodes.local = node // {
            nodeId = "../local";
          };
        };
        duplicateNodeIdentityRefused = rejected {
          nodes = {
            first = node // {
              nodeId = "same";
            };
            second = node // {
              nodeId = "same";
            };
          };
        };
        separateOwnersMayReuseNodeName = valid (nixos {
          inherit package;
          user = "operator";
          nodes = {
            first = node // {
              nodeId = "same";
            };
            second = node // {
              nodeId = "same";
              ownerDid = "did:key:z6MkOtherOwner";
            };
          };
        });
        missingOwnerRefused = rejected {
          nodes.local = node // {
            ownerDid = null;
          };
        };
        publishingPackageIsDefault =
          (nixos { }).services.herdr-supervisor.package
          == flake.packages.${pkgs.stdenv.hostPlatform.system}.herdr-supervisor;
        homePackageInstalled = builtins.elem package user.home.packages;
      };
  failed = builtins.attrNames (lib.filterAttrs (_: value: !value) report);
in
assert lib.assertMsg (
  failed == [ ]
) "Service module regressions: ${lib.concatStringsSep ", " failed}";
pkgs.runCommand "herdr-supervisor-service-module-checks"
  (
    {
      nativeBuildInputs = [ pkgs.ripgrep ];
    }
    // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      systemUnit =
        pkgs.writeText "nixos-controller.service"
          system.systemd.units."herdr-supervisor-controller.service".text;
      homeUnit = user.xdg.configFile."systemd/user/herdr-supervisor-controller.service".source;
    }
  )
  ''
    ${lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
      for unit in "$systemUnit" "$homeUnit"; do
        rg -F 'KillSignal=SIGINT' "$unit"
        rg -F 'KillMode=mixed' "$unit"
        rg -F 'TimeoutStopSec=infinity' "$unit"
        rg -F -- '--coordinator-config' "$unit"
        rg -F -- '/private/notifier.json' "$unit"
      done
      rg -F 'User=operator' "$systemUnit"
      if rg '^User=' "$homeUnit"; then exit 1; fi
    ''}
    cp ${pkgs.writeText "service-module-report.json" (builtins.toJSON report)} "$out"
  ''
