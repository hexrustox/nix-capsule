{ pkgs, nixCapsule }:
{
  mkShell =
    {
      image,
      devShell,
      socketPath,
      containerName,
      options ? [ ],
      removeOptions ? [ ],
      useDefaultOptions ? true,
      autoStart ? true,
      wait ? true,
      wrappers ? [ ],
      shellHook ? "",
    }:
    let
      lib = pkgs.lib;

      defaultOpts =
        let
          socketDir = builtins.dirOf socketPath;
        in
        lib.optionals useDefaultOptions [
          "--replace"
          "--name ${containerName}"
          "-v /nix:/nix"
          "-v /etc:/etc"
          "-v \"$project_root\":\"$project_root\""
          "-w \"$project_root\""
          "-e NCAP_SOCKET"
          "-v ${socketDir}:${socketDir}"
        ];

      finalOpts = lib.subtractLists removeOptions (defaultOpts ++ options);
      devShellPath =
        if (lib.hasPrefix "." devShell) || (lib.hasPrefix "/" devShell) then devShell else ".#${devShell}";

      startContainer =
        let
          blocking = "podman exec ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath} --command true";
        in
        pkgs.writeShellScriptBin "start-container" ''
          project_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)

          podman run -d \
            ${lib.concatStringsSep " " finalOpts} -- \
            ${image} \
            sleep infinity

          ${lib.optionalString wait blocking}

          podman exec -d ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath} --command ${nixCapsule}/bin/ncap-server
        '';

      stopContainer = pkgs.writeShellScriptBin "stop-container" ''
        podman stop -t 0 ${containerName}
      '';

      restartContainer = pkgs.writeShellScriptBin "restart-container" ''
        stop-container && start-container
      '';

      enterContainer = pkgs.writeShellScriptBin "enter-container" ''
        podman exec -it ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath}
      '';
    in
    pkgs.mkShellNoCC {
      packages = [
        nixCapsule
        startContainer
        stopContainer
        restartContainer
        enterContainer
      ]
      ++ (map ({ name, value }: pkgs.writeShellScriptBin name ''ncap ${value} "$@"'') (
        map (
          elem:
          if builtins.isString elem then
            {
              name = elem;
              value = elem;
            }
          else
            elem
        ) wrappers
      ));

      NCAP_SOCKET = socketPath;

      shellHook =
        let
          init = lib.optionalString autoStart ''
            if [[ "$(podman inspect -f '{{.State.Running}}' ${containerName} 2>/dev/null)" != "true" ]]; then
              start-container
            fi
          '';
        in
        ''
          ${init}

          ${shellHook}
        '';
    };
}
