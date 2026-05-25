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
      shellHook ? "",
      cachePath ? null,
    }:
    let
      socketDir = builtins.dirOf socketPath;

      devShellPath =
        if hasPrefix "." devShell || hasPrefix "/" devShell then devShell else ".#${devShell}";

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

      cacheDir = if cachePath == null then "$PROJECT_ROOT/.ncap-cache" else cachePath;

      ncapCtl = pkgs.writeShellScriptBin "ncap-ctl" ''
        set -euo pipefail

        PROJECT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
        CACHE_DIR="${cacheDir}"
        DEVSHELL="${devShellPath}"
        DEVSHELL_NAME=$(echo "$DEVSHELL" | sed 's/[^a-zA-Z0-9]/-/g; s/--*/-/g; s/^-//; s/-$//')
        CACHE_BASE="$CACHE_DIR/$DEVSHELL_NAME"
        CACHE="$CACHE_BASE/cache"
        CACHE_HASH="$CACHE_BASE/hash"
        PROFILE="$CACHE_BASE/profile"

        SOCKET_DIR="${socketDir}"
        SOCKET_PATH="${socketPath}"
        CONTAINER="${containerName}"
        IMAGE="${image}"
        RUNTIME="${runtime}"
        NCAP_INIT="${pkgs.ncap}/bin/ncap-init"
        NCAP_SERVER="${pkgs.ncap}/bin/ncap-server"
        NIX="${pkgs.nix}/bin/nix"
        BASH="${pkgs.bash}/bin/bash"

        is_running() {
          local state
          state=$("$RUNTIME" inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null || echo "false")
          [[ "$state" == "true" ]]
        }

        cmd_init() {
          local output new_hash old_hash
          mkdir -p "$CACHE_BASE"
          output=$("$NIX" print-dev-env --profile "$PROFILE" "$DEVSHELL")
          new_hash=$(echo "$output" | sha256sum | cut -d' ' -f1)
          old_hash=$(cat "$CACHE_HASH" 2>/dev/null || echo "")
          if [[ "$new_hash" == "$old_hash" ]] && [[ -f "$CACHE" ]]; then
            cmd_start
            return
          fi
          echo "Caching dev environment..." >&2
          echo "$output" > "$CACHE"
          echo "$new_hash" > "$CACHE_HASH"
          "$NIX" profile wipe-history --profile "$PROFILE"
          cmd_restart
        }

        cmd_start() {
          if is_running; then
            echo "Container '$CONTAINER' is already running." >&2
            return 0
          fi

          if [[ ! -f "$CACHE" ]]; then
            echo "No cached dev environment found. Run 'ncap-ctl init' first." >&2
            return 1
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

      shellHook = optionalString autoStart "ncap-ctl init\n" + shellHook;
    };
}
