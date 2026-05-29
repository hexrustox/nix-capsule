{ pkgs }:
let
  lib = pkgs.lib;
  inherit (lib)
    optionalString
    optionals
    concatStringsSep
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

      defaultOpts = [
        "--replace"
        "--name ${containerName}"
        "-v /nix:/nix:ro"
        "-v \"$project_root\":\"$project_root\""
        "-w \"$project_root\""
        "-v ${socketDir}:${socketDir}"
      ];

      hardenOpts = optionals harden [
        "--cap-drop=all"
        "--security-opt=no-new-privileges"
      ];

      finalOpts = concatStringsSep " " (defaultOpts ++ hardenOpts ++ extraOptions);

      ncapCtl = pkgs.writeShellScriptBin "ncap-ctl" ''
        set -euo pipefail

        project_root=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
        devshell="${devShellPath}"
        devshell_name=$(echo "$devshell" | sed 's/[^a-zA-Z0-9]/-/g; s/--*/-/g; s/^-//; s/-$//')
        cache_dir="$project_root/.ncap-cache/$devshell_name"
        cache_file="$cache_dir/env"
        nix_profile="$cache_dir/profile"

        socket_dir="${socketDir}"
        socket_path="${socketPath}"
        container="${containerName}"
        image="${image}"
        runtime="${runtime}"
        ncap_init_bin="${pkgs.ncap}/bin/ncap-init"
        ncap_server_bin="${pkgs.ncap}/bin/ncap-server"
        nix_bin="${pkgs.nix}/bin/nix"
        bash_bin="${pkgs.bash}/bin/bash"

        is_running() {
          local state
          state=$("$runtime" inspect -f '{{.State.Running}}' "$container" 2>/dev/null || echo "false")
          [[ "$state" == "true" ]]
        }

        cmd_init() {
          local output
          mkdir -p "$cache_dir"
          if [[ "''${NCAP_CACHE:-0}" -eq 0 ]] || [[ ! -f "$cache_file" ]]; then
            echo "Caching dev environment..." >&2
            output=$("$nix_bin" print-dev-env --profile "$nix_profile" "$devshell")
            echo "$output" > "$cache_file"
            "$nix_bin" profile wipe-history --profile "$nix_profile"
            cmd_restart
            return
          fi
          cmd_start
        }

        cmd_start() {
          if is_running; then
            echo "Container '$container' is already running." >&2
            return 0
          fi

          if [[ ! -f "$cache_file" ]]; then
            echo "No cached dev environment found. Run 'ncap-ctl init' first." >&2
            return 1
          fi

          mkdir -p "$socket_dir"

          "$runtime" run -d \
            ${finalOpts} -- \
            "$image" \
            "$ncap_init_bin" --socket "$socket_path"

          "$runtime" exec -d "$container" \
            "$bash_bin" -c "source $cache_file && exec $ncap_server_bin --socket $socket_path"
        }

        cmd_stop() {
          "$runtime" stop "$container"
        }

        cmd_restart() {
          cmd_stop
          cmd_start
        }

        cmd_enter() {
          if [[ ! -f "$cache_file" ]]; then
            echo "No cached dev environment found. Have you run 'start'?" >&2
            return 1
          fi
          exec "$runtime" exec -it "$container" \
            "$bash_bin" -c "source $cache_file; exec $bash_bin"
        }

        cmd_status() {
          if is_running; then
            echo "Container '$container' is running."
          else
            echo "Container '$container' is not running."
            return 1
          fi
        }

        case "''${1:-}" in
          init)    cmd_init ;;
          start)   cmd_start ;;
          stop)    cmd_stop ;;
          restart) cmd_restart ;;
          enter)   cmd_enter ;;
          status)  cmd_status ;;
          *)
            echo "usage: ncap-ctl {init|start|stop|restart|enter|status}" >&2
            exit 1
            ;;
        esac
      '';

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
        ncapCtl
      ]
      ++ map ({ name, value }: pkgs.writeShellScriptBin name ''exec ncap ${value} "$@"'') (
        map mkWrapperScript wrappers
      );

      NCAP_SOCKET = socketPath;

      shellHook = ''
        ${preShellHook}
        ${optionalString autoStart "ncap-ctl init"}
        ${postShellHook}
      '';
    };
}
