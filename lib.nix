{ pkgs }:
let
  lib = pkgs.lib;
  inherit (lib) optionalString optionals concatStringsSep hasPrefix;
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
      shellHook ? "",
      cachePath ? null,
    }:
    let
      socketDir = builtins.dirOf socketPath;

      devShellPath =
        if hasPrefix "." devShell || hasPrefix "/" devShell
        then devShell
        else ".#${devShell}";

      defaultOpts = [
        "--replace"
        "--name ${containerName}"
        "-v /nix:/nix"
        "-v /etc:/etc"
        "-v \"$PROJECT_ROOT\":\"$PROJECT_ROOT\""
        "-w \"$PROJECT_ROOT\""
        "-v ${socketDir}:${socketDir}"
      ];

      hardenOpts = optionals harden [
        "--cap-drop=all"
        "--security-opt=no-new-privileges"
      ];

      finalOpts = concatStringsSep " " (defaultOpts ++ hardenOpts ++ extraOptions);

      cacheDir =
        if cachePath == null
        then "$PROJECT_ROOT/.ncap-cache"
        else cachePath;

      ncapCtl = pkgs.writeShellScriptBin "ncap-ctl" ''
        set -euo pipefail

        PROJECT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
        CACHE_DIR="${cacheDir}"
        CACHE="$CACHE_DIR/cache"
        CACHE_HASH="$CACHE_DIR/hash"

        SOCKET_DIR="${socketDir}"
        SOCKET_PATH="${socketPath}"
        CONTAINER="${containerName}"
        IMAGE="${image}"
        RUNTIME="${runtime}"
        DEVSHELL="${devShellPath}"
        NCAP_INIT="${pkgs.ncap}/bin/ncap-init"
        NCAP_SERVER="${pkgs.ncap}/bin/ncap-server"
        NIX="${pkgs.nix}/bin/nix"
        BASH="${pkgs.bash}/bin/bash"

        cache_shell() {
          local need_cache=false
          local lock_file="$PROJECT_ROOT/flake.lock"

          if [[ -f "$lock_file" ]]; then
            local current_hash cached_hash
            current_hash=$(sha256sum "$lock_file" | cut -d' ' -f1)
            cached_hash=$(cat "$CACHE_HASH" 2>/dev/null || echo "")
            if [[ "$current_hash" != "$cached_hash" ]] || [[ ! -f "$CACHE" ]]; then
              need_cache=true
            fi
          elif [[ ! -f "$CACHE" ]]; then
            need_cache=true
          fi

          if $need_cache; then
            echo "Caching dev environment..." >&2
            mkdir -p "$CACHE_DIR"
            "$NIX" print-dev-env "$DEVSHELL" > "$CACHE" || {
              echo "Failed to evaluate dev shell: $DEVSHELL" >&2
              rm -f "$CACHE"
              return 1
            }
            [[ -f "$lock_file" ]] && echo "$current_hash" > "$CACHE_HASH"
          fi
        }

        is_running() {
          local state
          state=$("$RUNTIME" inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null || echo "false")
          [[ "$state" == "true" ]]
        }

        cmd_start() {
          cache_shell || return 1

          if is_running; then
            echo "Container '$CONTAINER' is already running." >&2
            return 0
          fi

          mkdir -p "$SOCKET_DIR"

          "$RUNTIME" run -d \
            ${finalOpts} -- \
            "$IMAGE" \
            "$NCAP_INIT" --socket "$SOCKET_PATH"

          "$RUNTIME" exec -d "$CONTAINER" \
            "$BASH" -c "source $CACHE && exec $NCAP_SERVER --socket $SOCKET_PATH"
        }

        cmd_stop() {
          "$RUNTIME" stop "$CONTAINER"
        }

        cmd_restart() {
          cmd_stop
          cmd_start
        }

        cmd_enter() {
          cache_shell || return 1
          if [[ ! -f "$CACHE" ]]; then
            echo "No cached dev environment found. Have you run 'start'?" >&2
            return 1
          fi
          exec "$RUNTIME" exec -it "$CONTAINER" \
            "$BASH" -c '[ -f ~/.bashrc ] && source ~/.bashrc; source '"$CACHE"'; exec '"$BASH"' --norc'
        }

        cmd_status() {
          if is_running; then
            echo "Container '$CONTAINER' is running."
          else
            echo "Container '$CONTAINER' is not running."
            return 1
          fi
        }

        case "''${1:-}" in
          start)   cmd_start ;;
          stop)    cmd_stop ;;
          restart) cmd_restart ;;
          enter)   cmd_enter ;;
          status)  cmd_status ;;
          *)
            echo "usage: ncap-ctl {start|stop|restart|enter|status}" >&2
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
      ] ++ map ({ name, value }: pkgs.writeShellScriptBin name ''exec ncap ${value} "$@"'') (
        map mkWrapperScript wrappers
      );

      NCAP_SOCKET = socketPath;

      shellHook = optionalString autoStart "ncap-ctl start\n" + shellHook;
    };
}
