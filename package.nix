{
  pkgs,
  rustPlatform ? pkgs.rustPlatform,
}:
let
  lib = pkgs.lib;
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
rustPlatform.buildRustPackage {
  pname = "ncap";
  version = cargoToml.package.version;
  inherit src;
  cargoLock.lockFile = ./Cargo.lock;

  stdenv = pkgs.clangStdenv;
  nativeBuildInputs = [ pkgs.mold ];

  doCheck = false;

  postInstall = ''
    mkdir -p $out/share/bash-completion/completions
    if [ -x $out/bin/ncap-ctl ]; then
      $out/bin/ncap-ctl completions bash > $out/share/bash-completion/completions/ncap-ctl 2>/dev/null || true
    fi
    if [ -x $out/bin/ncap ]; then
      $out/bin/ncap completions bash > $out/share/bash-completion/completions/ncap 2>/dev/null || true
    fi
    mkdir -p $out/share/zsh/site-functions
    if [ -x $out/bin/ncap-ctl ]; then
      $out/bin/ncap-ctl completions zsh > $out/share/zsh/site-functions/_ncap-ctl 2>/dev/null || true
    fi
    if [ -x $out/bin/ncap ]; then
      $out/bin/ncap completions zsh > $out/share/zsh/site-functions/_ncap 2>/dev/null || true
    fi
    mkdir -p $out/share/fish/vendor_completions.d
    if [ -x $out/bin/ncap-ctl ]; then
      $out/bin/ncap-ctl completions fish > $out/share/fish/vendor_completions.d/ncap-ctl.fish 2>/dev/null || true
    fi
    if [ -x $out/bin/ncap ]; then
      $out/bin/ncap completions fish > $out/share/fish/vendor_completions.d/ncap.fish 2>/dev/null || true
    fi
  '';
}
