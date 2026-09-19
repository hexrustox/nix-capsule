{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    { self, flake-parts, ... }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      flake = {
        lib = { pkgs }: import ./nix/lib.nix { inherit pkgs; };

        overlays = {
          default =
            final: prev:
            let
              system = prev.stdenv.targetPlatform.system;
              hashes = import ./nix/prebuilt-hashes.nix;
            in
            {
              ncap =
                if builtins.hasAttr system hashes then
                  self.packages.${system}.ncap-prebuilt
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
            ];
          };
          rust = pkgs.rust-bin.stable.${rustVersion}.default;
        in
        {
          packages = {
            default = pkgs.callPackage ./package.nix {
              rustPlatform = pkgs.makeRustPlatform {
                cargo = rust;
                rustc = rust;
              };
            };
            ncap-prebuilt = pkgs.callPackage ./nix/prebuilt.nix {
              inherit system;
            };
          };
        };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
    };
}
