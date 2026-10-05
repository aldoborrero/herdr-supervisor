{
  lib,
  pkgs,
  cfg,
}:
let
  # Reuse the pinned NixOS argument encoder for both module classes. It quotes
  # whitespace and escapes systemd percent specifiers and dollar expansion.
  utils = import (pkgs.path + "/nixos/lib/utils.nix") {
    inherit lib pkgs;
    config = { };
  };
  absolute =
    s: builtins.isString s && lib.hasPrefix "/" s && !(lib.hasInfix "\n" s || lib.hasInfix "\r" s);
  normalizedState =
    s:
    absolute s
    && s != "/"
    && s != builtins.storeDir
    && !(lib.hasPrefix "${builtins.storeDir}/" s)
    && lib.all (p: p != "" && p != "." && p != "..") (lib.tail (lib.splitString "/" s));
  validName = s: builtins.match "[A-Za-z0-9][A-Za-z0-9._-]{0,63}" s != null;
  nodes = lib.filterAttrs (_: n: n.enable) cfg.nodes;
  instances =
    lib.optionalAttrs cfg.controller.enable { controller = cfg.controller; }
    // lib.mapAttrs' (name: value: lib.nameValuePair "node-${name}" value) nodes;
  args = state: [
    (lib.getExe cfg.package)
    "control"
    "--state"
    state
  ];
  unit = name: instance: command: {
    inherit name;
    description = "Herdr supervisor ${name}";
    inherit (instance) after requires;
    serviceConfig = {
      Type = "simple";
      ExecStart = utils.escapeSystemdExecArgs command;
      UMask = "0077";
      Restart = "on-failure";
      # Keep automatic retries below StartLimitBurst/Interval even during a
      # prolonged broker outage, rather than permanently exhausting the limit.
      RestartSec = 15;
      KillSignal = "SIGINT";
      KillMode = "mixed";
      TimeoutStopSec = cfg.stopTimeout;
    };
  };
  nonnull = x: if x == null then "" else x;
  controller = cfg.controller;
  units =
    lib.optional controller.enable (
      unit "controller" controller (
        args controller.stateDirectory
        ++ [
          "run-controller"
          "--broker"
          (nonnull controller.brokerConfigFile)
          "--repositories"
          (nonnull controller.repositoriesConfigFile)
        ]
        ++ lib.optionals (controller.coordinatorConfigFile != null) [
          "--coordinator-config"
          controller.coordinatorConfigFile
        ]
      )
    )
    ++ lib.mapAttrsToList (
      name: node:
      unit "node-${name}" node (
        args node.stateDirectory
        ++ [
          "run-node"
          "--node"
          node.nodeId
          "--owner"
          (nonnull node.ownerDid)
          "--grant"
          (nonnull node.grantFile)
          "--broker"
          (nonnull node.brokerConfigFile)
          "--executor"
          (nonnull node.executorConfigFile)
          "--runtime-config"
          (nonnull node.runtimeConfigFile)
        ]
      )
    ) nodes;
  files =
    instance:
    [ instance.brokerConfigFile ]
    ++ (
      if instance ? nodeId then
        [
          instance.grantFile
          instance.executorConfigFile
          instance.runtimeConfigFile
        ]
      else
        [ instance.repositoriesConfigFile ]
        ++ lib.optional (instance.coordinatorConfigFile != null) instance.coordinatorConfigFile
    );
  states = map (i: i.stateDirectory) (builtins.attrValues instances);
  identities = map (
    n:
    builtins.toJSON [
      n.ownerDid
      n.nodeId
    ]
  ) (builtins.attrValues nodes);
in
{
  inherit units;
  enabled = units != [ ];
  assertions = [
    {
      assertion = pkgs.stdenv.hostPlatform.isLinux;
      message = "Herdr supervisor service modules require Linux/systemd.";
    }
    {
      assertion = builtins.length states == builtins.length (lib.unique states);
      message = "Herdr supervisor instances must use distinct state directories.";
    }
    {
      assertion = builtins.length identities == builtins.length (lib.unique identities);
      message = "Herdr supervisor node instances must not duplicate an enrolled owner/node identity.";
    }
  ]
  ++ lib.mapAttrsToList (name: instance: {
    assertion = normalizedState instance.stateDirectory && lib.all absolute (files instance);
    message = "Herdr supervisor ${name} needs a normalized private state directory outside /nix/store and absolute paths for all required configuration files.";
  }) instances
  ++ lib.mapAttrsToList (name: node: {
    assertion =
      validName name
      && validName node.nodeId
      && node.ownerDid != null
      && lib.hasPrefix "did:key:z" node.ownerDid;
    message = "Herdr supervisor node ${name} needs a valid instance/node ID and pinned owner did:key.";
  }) nodes;
}
