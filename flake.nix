{
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
      perSystem =
        {
          self',
          system,
          ...
        }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };
          capsule-lib = import ./lib.nix {
            inherit pkgs;
            nixCapsule = self'.packages.ncap;
          };
        in
        {
          packages.ncap = pkgs.callPackage ./package.nix { };

          devShells = {
            default = capsule-lib.mkShell {
              image = "ubuntu:latest";
              devShell = "container";
              socketPath = "/tmp/test";
              containerName = "ncap";
              opts = [
                "-e HOME"
                "-v \"$HOME/.cargo\":\"$HOME/.cargo\""
              ];
              wrappers = {
                codebook = "codebook";
                rust-analyzer = "rust-analyzer";
                nixfmt = "nixfmt";
                taplo = "taplo";
              };
            };

            container = pkgs.mkShellNoCC {
              packages = with pkgs; [
                cargo-deny
                cargo-edit
                cargo-machete
                clang
                codebook
                mold
                nixfmt
                taplo
                (rust-bin.stable."1.93.1".default.override {
                  extensions = [
                    "rust-src" # for rust-analyzer
                    "rust-analyzer"
                  ];
                })
              ];
            };
          };
        };

      systems = [ "x86_64-linux" ];
    };
}
