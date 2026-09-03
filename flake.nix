{
  description = "nix-capsule — containerized dev shells with transparent binary execution";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    { self, nixpkgs, rust-overlay, flake-parts, ... }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        overlays.default = final: prev:
          let
            rust = if prev ? rust-bin then prev.rust-bin.stable.${rustVersion}.default else null;
            rPlatform = if rust != null then prev.makeRustPlatform { cargo = rust; rustc = rust; } else prev.rustPlatform;
          in
          {
            ncap = prev.callPackage ./package.nix {
              pkgs = prev;
              rustPlatform = rPlatform;
            };
          };

        lib = { pkgs }: import ./lib.nix { inherit pkgs; };
      };

      perSystem =
        {
          system,
          ...
        }:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [
              rust-overlay.overlays.default
              self.overlays.default
            ];
          };
          capsule-lib = self.lib { inherit pkgs; };
          rust = pkgs.rust-bin.stable.${rustVersion}.default;
        in
        {
          packages = {
            ncap = pkgs.ncap;
            default = pkgs.ncap;
          };

          apps.default = capsule-lib.app;

          devShells = {
            default = capsule-lib.mkShell {
              project = "nix-capsule";
              image = "alpine:latest";
              devShell = "container";
              wrappers = [
                "cargo"
                "codebook-lsp"
                "rust-analyzer"
                "taplo"
              ];
              envForward = [ "CARGO_HOME" ];
              extraOptions = [
                "-e"
                "CARGO_HOME"
                "-v"
                "$CARGO_HOME:$CARGO_HOME"
              ];
              preShellHook = ''
                export CARGO_HOME=''${CARGO_HOME:-$HOME/.cargo}
                mkdir -p "$CARGO_HOME"
              '';
            };

            container =
              let
                rustWithExt = rust.override {
                  extensions = [
                    "rust-src"
                    "rust-analyzer"
                    "llvm-tools-preview"
                  ];
                };
              in
              pkgs.mkShellNoCC {
                packages = with pkgs; [
                  cargo-deny
                  cargo-edit
                  cargo-machete
                  cargo-llvm-cov
                  clang
                  codebook
                  mold
                  taplo
                  rustWithExt
                  git
                ];
              };
          };
        };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    };
}
