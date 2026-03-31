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
      wrappers ? { },
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
          "-e NCAP_SOCKET"
          "-v ${socketDir}:${socketDir}"
        ];

      finalOpts = lib.subtractLists removeOpts (defaultOpts ++ opts);

      startContainer = pkgs.writeShellScriptBin "start-container" ''
        project_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)

        podman run \
          ${lib.concatStringsSep " " finalOpts} -- \
          ${image} \
          ${pkgs.nix}/bin/nix develop .#${devShell} \
          --command ${nixCapsule}/bin/ncap-server
      '';

      stopContainer = pkgs.writeShellScriptBin "stop-container" ''
        podman stop -t 0 ${containerName}
      '';

      restartContainer = pkgs.writeShellScriptBin "restart-container" ''
        stop-container && start-container
      '';
    in
    pkgs.mkShellNoCC {
      packages = [
        nixCapsule
        startContainer
        stopContainer
        restartContainer
      ]
      ++ (map ({ name, value }: pkgs.writeShellScriptBin name ''ncap ${value} "$@"'') (
        lib.attrsToList wrappers
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
