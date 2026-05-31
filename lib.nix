{ pkgs }:
let
  lib = pkgs.lib;
  inherit (lib)
    optionalString
    optionals
    hasPrefix
    ;
in
{
  mkShell =
    {
      image,
      devShell,
      socketPath,
      containerName,
      runtime ? "podman",
      extraOptions ? [ ],
      harden ? false,
      autoStart ? true,
      wrappers ? [ ],
      preShellHook ? "",
      postShellHook ? "",
    }:
    let
      socketDir = builtins.dirOf socketPath;

      devShellPath =
        if hasPrefix "." devShell || hasPrefix "/" devShell then devShell else ".#${devShell}";

      mkWrapperScript =
        elem:
        if builtins.isString elem then
          {
            name = elem;
            value = elem;
          }
        else
          elem;
    in
    pkgs.mkShellNoCC {
      name = "nix-capsule";

      packages = [
        pkgs.ncap
      ]
      ++ map ({ name, value }: pkgs.writeShellScriptBin name ''exec ncap ${value} "$@"'') (
        map mkWrapperScript wrappers
      );

      NCAP_SOCKET = socketPath;
      NCAP_DEVSHELL = devShellPath;
      NCAP_CONTAINER = containerName;
      NCAP_IMAGE = image;
      NCAP_RUNTIME = runtime;
      NCAP_PODMAN_OPTS = builtins.toJSON (
        [
          "--replace"
          "--name"
          containerName
          "-v"
          "/nix:/nix:ro"
          "-v"
          "$PROJECT_ROOT:$PROJECT_ROOT"
          "-w"
          "$PROJECT_ROOT"
          "-v"
          "${socketDir}:${socketDir}"
        ]
        ++ optionals harden [
          "--cap-drop=all"
          "--security-opt=no-new-privileges"
        ]
        ++ extraOptions
      );
      NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
      NCAP_NIX = "${pkgs.nix}/bin/nix";
      NCAP_BASH = "${pkgs.bash}/bin/bash";

      shellHook = ''
        ${preShellHook}
        ${optionalString autoStart "ncap-ctl init"}
        ${postShellHook}
      '';
    };
}
