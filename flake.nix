{
  description = "Containerized dev shells with transparent binary execution";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    { flake-parts, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = { pkgs }: import ./lib.nix { inherit pkgs; };

        overlays.default = final: prev: {
          ncap = prev.callPackage ./package.nix { };
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
        in
        {
          packages.ncap = pkgs.callPackage ./package.nix { };

          devShells = {
            default = capsule-lib.mkShell {
              image = "ubuntu:latest";
              devShell = "container";
              socketPath = "/tmp/nix-capsule/ncap-socket";
              containerName = "ncap";
              options = [
                "-e HOME"
                "-e NIX_PATH"
                "-v \"$HOME/.cargo\":\"$HOME/.cargo\""
              ];
              wrappers = [
                "cargo"
                "codebook-lsp"
                "rust-analyzer"
                "nixd"
                "taplo"
              ];
            };

            container =
              let
                rust = (
                  pkgs.rust-bin.stable."1.93.1".default.override {
                    extensions = [
                      "rust-src" # for rust-analyzer
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
