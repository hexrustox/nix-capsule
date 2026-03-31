{ pkgs, nixCapsule }:
{
  mkShell =
    {
      image,
      devShell,
      socketPath,
      containerName,
      opts ? [ ],
      removeOpts ? [ ],
      autoStart ? true,
      wrappers ? [ ],
    }:
    let
      lib = pkgs.lib;

      defaultOpts =
        let
          socketDir = builtins.dirOf socketPath;
        in
        [
          "-d"
          "--replace"
          "--name ${containerName}"
          "-v /nix:/nix"
          "-v /etc:/etc"
          "-v \"$project_root\":\"$project_root\""
          "-w \"$project_root\""
          "-e PATH=${lib.makeBinPath [ pkgs.nix ]}"
          "-e NCAP_SOCKET"
          "-v ${socketDir}:${socketDir}"
        ];

      finalOpts = lib.subtractLists removeOpts (defaultOpts ++ opts);

      startContainer = pkgs.writeShellScriptBin "start-container" ''
        project_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)

        podman run \
          ${lib.concatStringsSep " " finalOpts} -- \
          ${image} \
          nix develop .#${devShell} \
          --command ${nixCapsule}/bin/ncap-server
      '';

      stopContainer = pkgs.writeShellScriptBin "stop-container" ''
        podman stop -t 0 ${containerName}
      '';

      restartContainer = pkgs.writeShellScriptBin "restart-container" ''
        stop-container && start-container
      '';

      enterContainer = pkgs.writeShellScriptBin "enter-container" ''
        podman exec -it ${containerName} /bin/sh
      '';

      containerLog = pkgs.writeShellScriptBin "container-log" ''
        podman logs ${containerName}
      '';
    in
    pkgs.mkShellNoCC {
      packages = [
        nixCapsule
        startContainer
        stopContainer
        restartContainer
        enterContainer
        containerLog
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
        '';
    };
}
