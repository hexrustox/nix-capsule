{ pkgs }:
let
  lib = pkgs.lib;

  wrapperSubmodule =
    { config, ... }:
    {
      options = {
        name = lib.mkOption { type = lib.types.str; };
        command = lib.mkOption {
          type = lib.types.str;
          default = config.name;
        };
        env = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
        };
        cwd = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
        };
      };
    };

  wrapperType = lib.types.coercedTo lib.types.str (name: { inherit name; }) (
    lib.types.submodule wrapperSubmodule
  );

  capsuleOptions = {
    project = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    image = lib.mkOption { type = lib.types.str; };
    devShell = lib.mkOption {
      type = lib.types.str;
      default = ".#container";
    };
    watchFiles = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [
        "flake.nix"
        "flake.lock"
      ];
    };
    envForward = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
    };
    wrappers = lib.mkOption {
      type = lib.types.listOf wrapperType;
      default = [ ];
    };
    extraOptions = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
    };
    harden = lib.mkOption {
      type = lib.types.bool;
      default = false;
    };
    timeout = lib.mkOption {
      type = lib.types.int;
      default = 10;
    };
    socketPath = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    containerName = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    cacheDir = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    logDir = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    logLevel = lib.mkOption {
      type = lib.types.str;
      default = "warning";
    };
    preShellHook = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    postShellHook = lib.mkOption {
      type = lib.types.str;
      default = "";
    };
    autoStart = lib.mkOption {
      type = lib.types.bool;
      default = true;
    };
    runtime = lib.mkOption {
      type = lib.types.str;
      default = "auto";
    };
    packages = lib.mkOption {
      type = lib.types.listOf lib.types.raw;
      default = [ ];
    };
    override = lib.mkOption {
      type = lib.types.attrsOf lib.types.raw;
      default = { };
    };
  };
in
{
  mkShell =
    { image, ... }@args:
    let
      unknownOpts = builtins.filter (opt: !(builtins.hasAttr opt capsuleOptions)) (
        builtins.attrNames args
      );

      checked =
        if unknownOpts != [ ] then
          throw "option `${builtins.head unknownOpts}`: unknown option"
        else
          (lib.evalModules {
            modules = [
              {
                options = capsuleOptions;
              }
              args
            ];
          }).config;

      mkWrapperScript =
        w:
        let
          envFlags = lib.concatMapStrings (e: " --env ${lib.escapeShellArg e}") w.env;
          cwdFlag = lib.optionalString (w.cwd != null) " --cwd ${lib.escapeShellArg w.cwd}";
          cmdArg = lib.escapeShellArg w.command;
        in
        pkgs.writeShellScriptBin w.name "exec ncap${envFlags}${cwdFlag} ${cmdArg} \"$@\"";

      wrapperBins = map mkWrapperScript checked.wrappers;

      watchFilesJson = builtins.toJSON checked.watchFiles;
      runOptsJson = builtins.toJSON checked.extraOptions;
      envForwardJson = builtins.toJSON checked.envForward;

      watchFileLines =
        lib.optionalString (checked.watchFiles != [ ])
          "command -v watch_file >/dev/null && watch_file ${
            lib.concatMapStringsSep " " lib.escapeShellArg checked.watchFiles
          }";

      setupEnvHook = ''
        ncap_setup_env="$(mktemp)"
        if ! ncap-ctl setup-env > "$ncap_setup_env"; then
          echo "ncap-ctl: setup-env failed (run \`ncap-ctl setup-env\` to retry)" >&2
        else
          source "$ncap_setup_env"
        fi
        rm -f "$ncap_setup_env"
      '';

      initHook = lib.optionalString checked.autoStart ''
        if ! ncap-ctl init; then
          echo "ncap-ctl: init failed (run \`ncap-ctl init\` to retry; wrapped commands will hint on connect)" >&2
        fi
      '';

      shellHookFragments = lib.concatStringsSep "\n" (
        lib.filter (s: s != "") [
          checked.preShellHook
          ''export NCAP_PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"''
          setupEnvHook
          watchFileLines
          initHook
          checked.postShellHook
        ]
      );
    in
    builtins.deepSeq checked (
      pkgs.mkShellNoCC {
        name = if checked.project == "" then "nix-capsule" else checked.project;

        NCAP_PROJECT = checked.project;
        NCAP_CONTAINER = checked.containerName;
        NCAP_SOCKET = checked.socketPath;
        NCAP_CACHE_DIR = checked.cacheDir;
        NCAP_LOG_DIR = checked.logDir;
        NCAP_LOG_LEVEL = checked.logLevel;
        NCAP_IMAGE = checked.image;
        NCAP_DEVSHELL = checked.devShell;
        NCAP_WATCH_FILES = watchFilesJson;
        NCAP_RUN_OPTS = runOptsJson;
        NCAP_ENV_FORWARD = envForwardJson;
        NCAP_TIMEOUT = toString checked.timeout;
        NCAP_HARDEN = if checked.harden then "true" else "false";
        NCAP_RUNTIME = checked.runtime;
        NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
        NCAP_NIX = "${pkgs.nix}/bin/nix";
        NCAP_BASH = "${pkgs.bash}/bin/bash";

        packages = [ pkgs.ncap ] ++ wrapperBins ++ checked.packages;

        shellHook = shellHookFragments;
      }
      // checked.override
    );

  devShellGuard = ''
    if [ ! -f /.dockerenv ] && [ ! -f /run/.containerenv ]; then
      echo "nix-capsule: this devshell must be entered inside a container (neither /.dockerenv nor /run/.containerenv exists)" >&2
      exit 1
    fi
  '';
}
