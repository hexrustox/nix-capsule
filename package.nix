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
  nativeBuildInputs = [ pkgs.mold ];

  doCheck = false;
}
