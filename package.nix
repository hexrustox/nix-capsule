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

  postInstall = ''
    mkdir -p $out/share/bash-completion/completions
    $out/bin/ncap-ctl completions bash \
      > $out/share/bash-completion/completions/ncap-ctl

    mkdir -p $out/share/zsh/site-functions
    $out/bin/ncap-ctl completions zsh \
      > $out/share/zsh/site-functions/_ncap-ctl

    mkdir -p $out/share/fish/vendor_completions.d
    $out/bin/ncap-ctl completions fish \
      > $out/share/fish/vendor_completions.d/ncap-ctl.fish
  '';
}
