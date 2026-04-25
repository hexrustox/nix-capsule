{ pkgs, nixCapsule }:
{
  mkShell =
    {
      image,
      devShell,
      socketPath,
      containerName,
      runtime ? "podman",
      options ? [ ],
      removeOptions ? [ ],
      useDefaultOptions ? true,
      hardenContainer ? false,
      autoStart ? true,
      wait ? true,
      nixShellName ? "nix-capsule",
      wrappers ? [ ],
      shellHook ? "",
    }:
    let
      lib = pkgs.lib;

      socketDir = builtins.dirOf socketPath;
      defaultOpts = lib.optionals useDefaultOptions [
        "--replace"
        "--name ${containerName}"
        "-v /nix:/nix"
        "-v /etc:/etc"
        "-v \"$project_root\":\"$project_root\""
        "-w \"$project_root\""
        "-e NCAP_SOCKET"
        "-v ${socketDir}:${socketDir}"
      ];
      hardenOpts = lib.optionals hardenContainer [
        "--cap-drop=all"
        "--security-opt=no-new-privileges"
      ];
      finalOpts = lib.subtractLists removeOptions (defaultOpts ++ hardenOpts ++ options);

      devShellPath =
        if (lib.hasPrefix "." devShell) || (lib.hasPrefix "/" devShell) then devShell else ".#${devShell}";

      startContainer =
        let
          blocking = "${runtime} exec -t ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath} --command true";
        in
        pkgs.writeShellScriptBin "start-container" ''
          project_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)

          mkdir -p ${socketDir}

          ${runtime} run -d \
            ${lib.concatStringsSep " " finalOpts} -- \
            ${image} \
            ${nixCapsule}/bin/ncap-init

          ${lib.optionalString wait blocking}

          ${runtime} exec -d ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath} --command ${nixCapsule}/bin/ncap-server
        '';

      stopContainer = pkgs.writeShellScriptBin "stop-container" ''
        ${runtime} stop ${containerName}
      '';

      restartContainer = pkgs.writeShellScriptBin "restart-container" ''
        stop-container && start-container
      '';

      enterContainer = pkgs.writeShellScriptBin "enter-container" ''
        ${runtime} exec -it ${containerName} ${pkgs.nix}/bin/nix develop ${devShellPath}
      '';
    in
    pkgs.mkShellNoCC {
      name = nixShellName;

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
            if [[ "$(${runtime} inspect -f '{{.State.Running}}' ${containerName} 2>/dev/null)" != "true" ]]; then
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
