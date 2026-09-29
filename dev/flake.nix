{
  inputs = {
    root.url = "path:../";
    nixpkgs.follows = "root/nixpkgs";
    rust-overlay.follows = "root/rust-overlay";
    flake-parts.follows = "root/flake-parts";
    nix-unit = {
      url = "github:nix-community/nix-unit";
      inputs.nixpkgs.follows = "root/nixpkgs";
    };
    nix-capsule.url = "github:hexrustox/nix-capsule?ref=v0.11.1";
  };

  outputs =
    {
      self,
      flake-parts,
      nix-unit,
      ...
    }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        nix-unit.modules.flake.default
      ];
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
          devShells = {
            default = capsule-lib.mkShell {
              image = "alpine:latest";
              watchFiles = [
                "flake.nix"
                "flake.lock"
                "dev/flake.nix"
                "dev/flake.lock"
              ];
              devShell = "./dev#container";
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
                "rust-analyzer"
                "nixd"
                "taplo"
                "typos"
                {
                  name = "nix-unit";
                  command = "nix-unit";
                  env = [ "NIX_CONFIG=experimental-features = nix-command flakes" ];
                }
              ];
              preShellHook = ''
                export CARGO_HOME=''${CARGO_HOME:-$HOME/.cargo}
                mkdir -p "$CARGO_HOME"
              '';
            };
            container = pkgs.mkShellNoCC {
              packages = with pkgs; [
                (rust-bin.stable.${rustVersion}.default.override {
                  extensions = [
                    "rust-src"
                    "rust-analyzer"
                    "llvm-tools-preview"
                  ];
                })
                cargo-deny
                cargo-edit
                cargo-machete
                cargo-llvm-cov
                clang
                mold

                taplo

                nixd
                nixfmt
                pkgs.nix-unit

                typos

                skills
                git
              ];
            };
          };

          nix-unit.inputs = {
            inherit (inputs)
              root
              nixpkgs
              flake-parts
              rust-overlay
              nix-capsule
              nix-unit
              ;
          };

          nix-unit.tests = import "${inputs.root}/nix/tests.nix" {
            mkNcapPkgs =
              overlays:
              import inputs.nixpkgs {
                inherit system;
                overlays = overlays;
              };
          };
        };

      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
    };
}
