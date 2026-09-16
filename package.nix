{
  pkgs,
  rustPlatform ? pkgs.rustPlatform,
}:
let
  lib = pkgs.lib;
  cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };
in
rustPlatform.buildRustPackage {
  pname = "ncap";
  version = cargoToml.package.version;
  inherit src;
  cargoLock.lockFile = ./Cargo.lock;

  stdenv = pkgs.clangStdenv;
  nativeBuildInputs = [
    pkgs.mold
    pkgs.installShellFiles
  ];

  doCheck = false;

  # Build-time helper: `ncap-completions` emits the completion scripts and is
  # dropped from the package output right after.
  postInstall = ''
    for bin in ncap ncap-ctl; do
      for shell in bash zsh fish; do
        installShellCompletion --cmd "$bin" \
          --"$shell" <("$out/bin/ncap-completions" "$bin" "$shell")
      done
    done
    rm "$out/bin/ncap-completions"
  '';
}
