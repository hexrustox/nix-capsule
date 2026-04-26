{
  pkgs,
}:
let
  inherit (pkgs) lib;

  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };
in
pkgs.rustPlatform.buildRustPackage {
  inherit src;
  pname = "ncap";
  version = cargoToml.package.version;
  cargoLock = {
    lockFile = src + /Cargo.lock;
  };
  doCheck = false;

  stdenv = pkgs.clangStdenv;
  nativeBuildInputs = [ pkgs.mold ];
}
