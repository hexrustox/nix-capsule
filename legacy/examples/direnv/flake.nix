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
              inputs.nix-capsule.overlays.default
            ];
          };
          capsule-lib = inputs.nix-capsule.lib { inherit pkgs; };
        in
        {
          apps.default = capsule-lib.app;
          devShells = {
            default = capsule-lib.mkShell {
              image = "alpine:latest";
              devShell = "container";
              socketPath = "/tmp/nix-capsule-examples-direnv/ncap-socket";
              containerName = "nix-capsule-examples-direnv";
              extraOptions = [
              ];
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
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    };
}
