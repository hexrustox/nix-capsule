{
  pkgs ? import <nixpkgs> { },
}:
let
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter =
      path: type:
      let
        base = builtins.baseNameOf path;
      in
      builtins.elem base [
        "src"
        "Cargo.toml"
        "Cargo.lock"
        ".cargo"
      ];
  };
in
pkgs.rustPlatform.buildRustPackage {
  inherit src;
  pname = "ncap";
  version = "0.1.0";
  cargoLock = {
    lockFile = src + /Cargo.lock;
  };
  doCheck = false;

  stdenv = pkgs.clangStdenv;
  nativeBuildInputs = [ pkgs.mold ];
}
