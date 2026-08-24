{
  inputs,
  extraOverlays ? (_: [ ]),
  devShell ? { },
  container ? (pkgs: pkgs.mkShell { }),
}:
let
  flake-parts = inputs.flake-parts;
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
          inputs.nix-capsule.overlays.default
        ];
      };
      capsule-lib = inputs.nix-capsule.lib { inherit pkgs; };
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
