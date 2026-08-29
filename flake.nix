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
    inputs:
    (import ./template.nix) {
      inherit inputs;

      extraOverlays = inputs: [
        inputs.rust-overlay.overlays.default
      ];

      devShell = {
        image = "ubuntu:latest";
        socketPath = "/tmp/nix-capsule/ncap-socket";
        containerName = "nix-capsule";
        extraOptions = [
          "-e"
          "CARGO_HOME"
          "-v"
          "$CARGO_HOME:$CARGO_HOME"
        ];
        wrappers = [
          "cargo"
          "codebook-lsp"
          "rust-analyzer"
          "taplo"
        ];
        preShellHook = ''
          export CARGO_HOME=''${CARGO_HOME:-$HOME/.cargo}
          mkdir -p "$CARGO_HOME"
        '';
      };

      container =
        pkgs:
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
            (rust-bin.stable."1.95.0".default.override {
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
}
