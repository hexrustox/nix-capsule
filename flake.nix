{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
    nix-capsule.url = "gitlab:codnixus/nix-capsule?ref=v0.1.0";
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
        in
        {
          packages.ncap = pkgs.callPackage ./package.nix { };

          lib =
            args:
            import ./lib.nix (
              args
              // {
                nixCapsule = self'.packages.ncap;
              }
            );

          devShells = {
            default = (inputs.nix-capsule.lib.${system} { inherit pkgs; }).mkShell {
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

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    };
}
