{
  inputs = {
    nix-capsule.url = "path:../../";
    nixpkgs.follows = "nix-capsule/nixpkgs";
    flake-parts.follows = "nix-capsule/flake-parts";
  };

  outputs =
    { flake-parts, ... }@inputs:
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
              inputs.nix-capsule.overlays.from-source
            ];
          };
          capsule-lib = inputs.nix-capsule.lib { inherit pkgs; };
        in
        {
          devShells = {
            default = capsule-lib.mkShell {
              project = "nix-capsule-example-simple";
              wrappers = [
                "cowsay"
              ];
              postShellHook = ''
                echo Welcome to nix capsule devshell
              '';
            };

            container = pkgs.mkShellNoCC {
              packages = with pkgs; [
                cowsay
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
