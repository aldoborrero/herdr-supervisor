{
  flake,
  inputs,
  pkgs,
  ...
}:
let
  mod = inputs.treefmt-nix.lib.evalModule pkgs {
    projectRootFile = "flake.nix";
    programs = {
      deadnix.enable = true;
      deadnix.no-lambda-pattern-names = true;
      deadnix.priority = 1;
      nixfmt.enable = true;
      nixfmt.priority = 2;
      rustfmt.enable = true;
      ruff-check.enable = true;
      ruff-check.priority = 1;
      ruff-format.enable = true;
      ruff-format.priority = 2;
      yamlfmt.enable = true;
      jsonfmt.enable = true;
      mdformat.enable = true;
      just.enable = true;
    };
    settings.excludes = [
      "target/**"
      ".cargo-home/**"
      "docs/upstream/**"
      "CLAUDE.md"
    ];
  };
  wrapper = mod.config.build.wrapper;
in
wrapper
// {
  passthru = wrapper.passthru // {
    tests.check = mod.config.build.check flake;
  };
}
