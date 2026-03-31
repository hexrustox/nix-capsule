{
  pkgs ? import <nixpkgs> { },
}:
let
  lib = pkgs.lib;
  src =
    let
      src = ./.;
    in
    lib.cleanSourceWith {
      inherit src;
      filter =
        path: type:
        let
          path' = lib.removePrefix (builtins.toString src) path;
          f = lib.hasPrefix "/src" path' || lib.hasPrefix "/.cargo" path' || lib.hasPrefix "/Cargo." path';
        in
        f;
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
