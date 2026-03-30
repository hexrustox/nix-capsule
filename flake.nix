{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
    capsule.url = "gitlab:codnixus/capsule";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      capsule,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { overlays = [ rust-overlay.overlays.default ]; };
        host-pkgs = import <nixpkgs> { };
        capsule-lib = capsule.lib {
          inherit pkgs;
          name = "temp";
          devTools =
            with host-pkgs;
            [
              nil
              nixfmt
              taplo
              {
                pkg = opencode;
                extraOpts = [
                  "-t"
                ];
              }
            ]
            ++ (with pkgs; [
              {
                pkg = codebook;
                name = "codebook-lsp";
              }
              {
                name = "rust-analyzer";
              }
              {
                name = "cargo";
                extraOpts = [
                  "-t"
                  "--workdir=$(pwd)"
                ];
              }
            ]);
          runtimeDeps =
            with host-pkgs;
            [ nix ]
            ++ (with pkgs; [
              clang
              mold
              (rust-bin.stable."1.93.1".default.override {
                extensions = [
                  "rust-src" # for rust-analyzer
                  "rust-analyzer"
                ];
              })

              cargo-deny
              cargo-edit
              cargo-machete
            ]);
          extraOpts = [
            "--pid host"
            "--uts host"

            "--env=HOME"

            "--env=COLORTERM=truecolor"
            "--env=TERM=xterm-256color"

            "--tmpfs=/tmp"

            "--volume=/etc/static/ssl/certs:/etc/ssl/certs:ro"

            "--volume=\"$HOME/.cargo\":\"$HOME/.cargo\""
          ];
          removeOpts = ["--cap-drop=all"];
          image = "ubuntu:latest";
        };
      in
      {
        devShells.default =
          pkgs.mkShellNoCC {
            inherit (capsule-lib) packages;
            shellHook = ''
              ${capsule-lib.shellHook}
            '';
          };
      }
    );
}
