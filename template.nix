{
  inputs,
  extraOverlays ? (_: [ ]),
  devShell ? { },
  container ? (pkgs: pkgs.mkShell { }),
}:
let
  flake-parts = inputs.flake-parts;
  # Support both `nix-capsule` input name and `self` (when this repo is the capsule).
  capsuleInput = inputs.nix-capsule or inputs.self or (throw "template.nix: expected `inputs.nix-capsule` or `inputs.self`");
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
        overlays = extraOverlays inputs ++ [
          capsuleInput.overlays.default
        ];
      };
      capsule-lib = capsuleInput.lib { inherit pkgs; };
    in
    {
      apps.default = capsule-lib.app;
      devShells = {
        default = capsule-lib.mkShell (
          {
            image = "alpine:latest";
            devShell = "container";
          }
          // devShell
        );
        container = container pkgs;
      };
    };

  systems = [
    "x86_64-linux"
    "aarch64-linux"
    "x86_64-darwin"
    "aarch64-darwin"
  ];
}
