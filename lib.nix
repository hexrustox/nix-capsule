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
      logDir ? null,
      containerName,
      runtime ? "podman",
      extraOptions ? [ ],
      extraPackages ? [ ],
      harden ? false,
      autoStart ? true,
      timeout ? 10,
      wrappers ? [ ],
      preShellHook ? "",
      postShellHook ? "",
    }:
    let
      socketDir = dirOf socketPath;

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
      )
      ++ extraPackages;

      NCAP_SOCKET = socketPath;
      NCAP_LOG_DIR = "${if !isNull logDir then logDir else socketDir}";
      NCAP_DEVSHELL = devShellPath;
      NCAP_CONTAINER = containerName;
      NCAP_IMAGE = image;
      NCAP_RUNTIME = runtime;
      NCAP_RUN_OPTS = builtins.toJSON (
        optionals harden [
          "--cap-drop=all"
          "--security-opt=no-new-privileges"
        ]
        ++ extraOptions
      );
      NCAP_TIMEOUT = toString timeout;
      NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
      NCAP_NIX = "${pkgs.nix}/bin/nix";
      NCAP_BASH = "${pkgs.bash}/bin/bash";

      shellHook = ''
        ${preShellHook}
        export PROJECT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
        ${optionalString autoStart "ncap-ctl init"}
        ${postShellHook}
      '';
    };

  app = {
    type = "app";
    program = "${pkgs.ncap}/bin/ncap-direnv";
    meta = {
      description = "direnv integration for nix-capsule cache validation";
    };
  };
}
