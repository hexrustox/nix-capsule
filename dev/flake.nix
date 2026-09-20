{
  inputs = {
    root.url = "path:../";
    nixpkgs.follows = "root/nixpkgs";
    rust-overlay.follows = "root/rust-overlay";
    flake-parts.follows = "root/flake-parts";
    nix-capsule.url = "github:hexrustox/nix-capsule?ref=v0.11.1";
  };

  outputs =
    { self, flake-parts, ... }@inputs:
    let
      rustVersion = "1.95.0";
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
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
              watchFiles = ["flake.nix" "flake.lock" "dev/flake.nix" "dev/flake.lock"];
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

                typos

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
