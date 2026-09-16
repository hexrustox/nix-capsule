{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
    nix-capsule.url = "gitlab:codnixus/nix-capsule?ref=v0.8.0";
  };

  outputs =
    { self, flake-parts, ... }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = { pkgs }: import ./lib.nix { inherit pkgs; };

        overlays = {
          default =
            final: prev:
            let
              system = prev.stdenv.targetPlatform.system;
            in
            {
              ncap =
                if system == "x86_64-linux" then
                  self.packages.x86_64-linux.ncap-prebuilt
                else
                  self.packages.${system}.default;
            };
          from-source =
            final: prev:
            let
              system = prev.stdenv.targetPlatform.system;
            in
            {
              ncap = self.packages.${system}.default;
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
              inputs.nix-capsule.overlays.default
            ];
          };
          capsule-lib = inputs.nix-capsule.lib { inherit pkgs; };
        in
        {
          apps.default = capsule-lib.app;
          packages =
            let
              pkgs = import inputs.nixpkgs {
                inherit system;
                overlays = [
                  inputs.rust-overlay.overlays.default
                ];
              };
              rust = pkgs.rust-bin.stable.${rustVersion}.default;
            in
            {
              default = pkgs.callPackage ./package.nix {
                rustPlatform = pkgs.makeRustPlatform {
                  cargo = rust;
                  rustc = rust;
                };
              };
              ncap-prebuilt =
                if system == "x86_64-linux" then
                  pkgs.callPackage ./prebuilt.nix { }
                else
                  throw "ncap-prebuilt: no prebuilt artifact for ${system}; use packages.${system}.default";
            };

          devShells = {
            default = capsule-lib.mkShell {
              socketPath = "/tmp/nix-capsule/ncap-socket";
              containerName = "nix-capsule";
              image = "alpine:latest";
              devShell = "container";
              extraOptions = [
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
            container = pkgs.mkShellNoCC {
              packages = with pkgs; [
                cargo-deny
                cargo-edit
                cargo-machete
                cargo-llvm-cov
                clang
                codebook
                nixd
                nixfmt
                mold
                taplo
                (rust-bin.stable.${rustVersion}.default.override {
                  extensions = [
                    "rust-src"
                    "rust-analyzer"
                    "llvm-tools-preview"
                  ];
                })
                skills
                git
              ];
            };
          };
        };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
    };
}
