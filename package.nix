{
  pkgs ? import <nixpkgs> { },
}:
let
  lib = pkgs.lib;
  src = lib.sourceByRegex ./. [
    "Cargo\.lock"
    "Cargo\.toml"
    "src"
    ".*\.rs$"
  ];
in
pkgs.rustPlatform.buildRustPackage {
  inherit src;
  pname = "ncap";
  version = "0.2.0";
  cargoLock = {
    lockFile = src + /Cargo.lock;
  };
  doCheck = false;

  stdenv = pkgs.clangStdenv;
  nativeBuildInputs = [ pkgs.mold ];
}
