{
  description = "Containerized dev shells with transparent binary execution";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-26.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    { flake-parts, ... }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = { pkgs }: import ./lib.nix { inherit pkgs; };

        overlays.default = final: prev: {
          ncap = prev.callPackage ./release.nix {
            system = prev.stdenv.targetPlatform.system;
          };
        };
      };

      perSystem =
        {
          system,
          ...
        }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [
              inputs.rust-overlay.overlays.default
              inputs.self.overlays.default
            ];
          };
          capsule-lib = inputs.self.lib { inherit pkgs; };
          rust = pkgs.rust-bin.stable.${rustVersion}.default;
        in
        {
          packages = {
            ncap = pkgs.callPackage ./package.nix {
              rustPlatform = pkgs.makeRustPlatform {
                cargo = rust;
                rustc = rust;
              };
            };
          };

          apps.default = capsule-lib.app;

          devShells = {
            default = capsule-lib.mkShell {
              image = "alpine:latest";
              devShell = "container";
              socketPath = "/tmp/nix-capsule/ncap-socket";
              containerName = "ncap";
              extraOptions = [
                "-e"
                "HOME"
                "-e"
                "NIX_PATH"
                "-e"
                "CARGO_HOME"
                "-v"
                "$CARGO_HOME:$CARGO_HOME"
              ];
              wrappers = [
                "cargo"
                "codebook-lsp"
                "rust-analyzer"
                "nixd"
                "taplo"
              ];
              preShellHook = ''
                export CARGO_HOME=''${CARGO_HOME:-$HOME/.cargo}
                mkdir -p "$CARGO_HOME"
              '';
            };

            container =
              let
                rust = (
                  pkgs.rust-bin.stable.${rustVersion}.default.override {
                    extensions = [
                      "rust-src"
                      "rust-analyzer"
                    ];
                  }
                );
              in
              pkgs.mkShellNoCC {
                packages = with pkgs; [
                  cargo-deny
                  cargo-edit
                  cargo-machete
                  clang
                  codebook
                  mold
                  nixd
                  nixfmt
                  rust
                  taplo

                  skills
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
