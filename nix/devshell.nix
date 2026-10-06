{ pkgs, perSystem }:
perSystem.devshell.mkShell {
  packages = with pkgs; [
    just
    perSystem.self.formatter
    # Rust compiler, formatter and lints for the supervisor.
    rustc
    cargo
    rustfmt
    clippy
    # Actual repository and broker integration tests, without any model processes.
    git
    nats-server
    process-compose
    python3
    openssh
    rsync
  ];
  env = [
    {
      name = "NIX_PATH";
      value = "nixpkgs=${toString pkgs.path}";
    }
    {
      name = "NIX_DIR";
      eval = "$PRJ_ROOT/nix";
    }
    {
      name = "CARGO_HOME";
      eval = "$PRJ_ROOT/.cargo-home";
    }
    {
      name = "NATS_SERVER";
      value = "${pkgs.nats-server}/bin/nats-server";
    }
  ];
}
