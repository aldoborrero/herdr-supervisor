{
  lib,
  src,
  rustPlatform,
  makeWrapper,
  git,
  openssh,
  rsync,
  coreutils,
  bash,
  nats-server,
  gh,
  tzdata,
}:
rustPlatform.buildRustPackage {
  pname = "herdr-supervisor";
  version = "0.1.0";
  inherit src;
  cargoLock.lockFile = src + "/Cargo.lock";
  nativeBuildInputs = [ makeWrapper ];
  nativeCheckInputs = [
    git
    openssh
    rsync
    coreutils
    bash
    nats-server
  ];
  HERDR_SUPERVISOR_BUILD_ID = "source-${
    builtins.substring 0 12 (builtins.hashString "sha256" (toString src))
  }";
  NATS_SERVER = "${nats-server}/bin/nats-server";
  TZDIR = "${tzdata}/share/zoneinfo";
  preCheck = ''
    export HOME="$TMPDIR/test-home"
    mkdir -p "$HOME"
  '';
  postInstall = ''
    mkdir -p $out/share/herdr-supervisor/skill $out/share/herdr-supervisor/docs
    cp -r skill/. $out/share/herdr-supervisor/skill/
    cp LICENSE NOTICE README.md $out/share/herdr-supervisor/
    cp -r docs/. $out/share/herdr-supervisor/docs/
    mkdir -p $out/share/herdr-supervisor/assets
    cp assets/supervisor-banner.svg $out/share/herdr-supervisor/assets/
    cp herdr-plugin.toml $out/share/herdr-supervisor/
    substituteInPlace $out/share/herdr-supervisor/herdr-plugin.toml \
      --replace-fail '"target/release/herdr-supervisor"' '"'$out'/bin/herdr-supervisor"'
    # Nix supplies the compiled binary; Herdr must not rebuild an immutable store path.
    sed -i '/^\[\[build\]\]/,/^command = /d' $out/share/herdr-supervisor/herdr-plugin.toml
    wrapProgram $out/bin/herdr-supervisor \
      --set-default TZDIR "${tzdata}/share/zoneinfo" \
      --prefix PATH : ${
        lib.makeBinPath [
          git
          openssh
          rsync
          coreutils
          bash
          gh
        ]
      }
  '';
  meta = {
    description = "Durable multi-host supervision for Herdr";
    license = lib.licenses.mit;
    sourceProvenance = with lib.sourceTypes; [ fromSource ];
    maintainers = with lib.maintainers; [ aldoborrero ];
    mainProgram = "herdr-supervisor";
    platforms = lib.platforms.unix;
  };
}
