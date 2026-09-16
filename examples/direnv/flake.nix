{
  inputs = {
    nix-capsule.url = "path:../../";
    nixpkgs.follows = "nix-capsule/nixpkgs";
  };

  outputs =
    { ... }@inputs:
    let
      system = "x86_64-linux";
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [
          inputs.nix-capsule.overlays.from-source
        ];
      };
      capsule-lib = inputs.nix-capsule.lib { inherit pkgs; };
    in
    {
      devShells.${system} = {
        default = capsule-lib.mkShell {
          project = "nix-capsule-example-direnv";
          image = "alpine:latest";
          wrappers = [
            "hello"
          ];
          postShellHook = ''
            echo Welcome to nix capsule devshell
          '';
        };

        container = pkgs.mkShellNoCC {
          packages = with pkgs; [
            hello
          ];
          shellHook = ''
            ${capsule-lib.devShellGuard}
          '';
        };
      };

    };
}
