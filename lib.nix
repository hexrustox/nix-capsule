{ pkgs }:
let
  lib = pkgs.lib;

  # Render a received value for error messages: type + value.
  showReceived =
    v:
    if v == null then
      "null"
    else if builtins.isString v then
      "string `\"${v}\"`"
    else if builtins.isPath v then
      "nix path `${toString v}`"
    else
      "${builtins.typeOf v} `${toString v}`";

  throwOpt =
    opt: expected: v:
    throw "option `${opt}`: expected ${expected}, got ${showReceived v}";

  # ---- scalar checks (return the value on success) ---------------------------
  checkString = opt: v: if builtins.isString v then v else throwOpt opt "string" v;

  checkNullOrString =
    opt: v: if v == null || builtins.isString v then v else throwOpt opt "null or string" v;

  checkBool = opt: v: if builtins.isBool v then v else throwOpt opt "bool" v;

  checkInt = opt: v: if builtins.isInt v then v else throwOpt opt "integer" v;

  # ---- list-of-strings check (returns the list on success) -------------------
  checkStringList =
    opt: v:
    if !builtins.isList v then
      throwOpt opt "list of strings" v
    else
      map (
        e:
        if builtins.isString e then
          e
        else
          throw "option `${opt}`: expected list of strings, got entry ${showReceived e}"
      ) v;

  # ---- wrappers check --------------------------------------------------------
  checkWrappers =
    opt: v:
    if !builtins.isList v then
      throwOpt opt "list of strings or attrsets" v
    else
      map (
        elem:
        if builtins.isString elem then
          elem
        else if builtins.isAttrs elem then
          let
            hasName = elem ? name;
            nameRaw =
              if hasName then elem.name else throw "option `${opt}`: wrapper attrset missing required `name`";
            name =
              if builtins.isString nameRaw then
                nameRaw
              else
                throw "option `${opt}`: expected wrapper `name` to be string, got ${showReceived nameRaw}";
            commandRaw = if elem ? command then elem.command else name;
            command =
              if builtins.isString commandRaw then
                commandRaw
              else
                throw "option `${opt}`: expected wrapper `command` to be string, got ${showReceived commandRaw}";
            envRaw = if elem ? env then elem.env else [ ];
            env =
              if !builtins.isList envRaw then
                throw "option `${opt}`: expected wrapper `env` to be list of strings, got ${showReceived envRaw}"
              else
                map (
                  e:
                  if builtins.isString e then
                    e
                  else
                    throw "option `${opt}`: expected wrapper `env` to be list of strings, got entry ${showReceived e}"
                ) envRaw;
            cwdRaw = if elem ? cwd then elem.cwd else null;
            cwd =
              if cwdRaw == null || builtins.isString cwdRaw then
                cwdRaw
              else
                throw "option `${opt}`: expected wrapper `cwd` to be null or string, got ${showReceived cwdRaw}";
          in
          builtins.deepSeq [ name command env cwd ] elem
        else
          throw "option `${opt}`: expected string or attrset, got ${showReceived elem}"
      ) v;
in
{
  mkShell =
    {
      project ? null,
      image ? "alpine:latest",
      devShell ? ".#container",
      watchFiles ? [
        "flake.nix"
        "flake.lock"
      ],
      envForward ? [ ],
      wrappers ? [ ],
      extraOptions ? [ ],
      harden ? false,
      timeout ? 10,
      socketPath ? null,
      containerName ? null,
      cacheDir ? null,
      logDir ? null,
      preShellHook ? "",
      postShellHook ? "",
      autoStart ? true,
      runtime ? "podman",
    }:
    let
      # ---- eval-time type checks (spec/flake-api.md § Type checks, ADR-0003) --
      # Every option is checked before mkShellNoCC runs; a mismatch throws
      # naming the option, the expected shape, and the received type/value.
      # Null is accepted only where the table default is null. No coercions;
      # no Nix `path` values for any option.
      checkedProject = checkNullOrString "project" project;
      checkedImage = checkString "image" image;
      checkedDevShell = checkString "devShell" devShell;
      checkedWatchFiles = checkStringList "watchFiles" watchFiles;
      checkedEnvForward = checkStringList "envForward" envForward;
      checkedWrappers = checkWrappers "wrappers" wrappers;
      checkedExtraOptions = checkStringList "extraOptions" extraOptions;
      checkedHarden = checkBool "harden" harden;
      checkedTimeout = checkInt "timeout" timeout;
      checkedSocketPath = checkNullOrString "socketPath" socketPath;
      checkedContainerName = checkNullOrString "containerName" containerName;
      checkedCacheDir = checkNullOrString "cacheDir" cacheDir;
      checkedLogDir = checkNullOrString "logDir" logDir;
      checkedPreShellHook = checkString "preShellHook" preShellHook;
      checkedPostShellHook = checkString "postShellHook" postShellHook;
      checkedAutoStart = checkBool "autoStart" autoStart;
      checkedRuntime = checkString "runtime" runtime;

      # Force all checks before building the shell. deepSeq catches lazy
      # list entries (plain seq only forces the list spine).
      checkAll = builtins.deepSeq [
        checkedProject
        checkedImage
        checkedDevShell
        checkedWatchFiles
        checkedEnvForward
        checkedWrappers
        checkedExtraOptions
        checkedHarden
        checkedTimeout
        checkedSocketPath
        checkedContainerName
        checkedCacheDir
        checkedLogDir
        checkedPreShellHook
        checkedPostShellHook
        checkedAutoStart
        checkedRuntime
      ] true;

      # ---- wrappers normalization ----------------------------------------------
      normalizedWrappers = map (
        elem:
        if builtins.isString elem then
          {
            name = elem;
            command = elem;
            env = [ ];
            cwd = null;
          }
        else
          {
            name = elem.name;
            command = if elem ? command then elem.command else elem.name;
            env = if elem ? env then elem.env else [ ];
            cwd = if elem ? cwd then elem.cwd else null;
          }
      ) checkedWrappers;

      mkWrapperScript =
        w:
        let
          envFlags = lib.concatMapStrings (e: " --env ${lib.escapeShellArg e}") w.env;
          cwdFlag = lib.optionalString (w.cwd != null) " --cwd ${lib.escapeShellArg w.cwd}";
          cmdArg = lib.escapeShellArg w.command;
        in
        pkgs.writeShellScriptBin w.name "exec ncap${envFlags}${cwdFlag} ${cmdArg} \"$@\"";

      wrapperBins = map mkWrapperScript normalizedWrappers;

      # ---- JSON-array vars ----------------------------------------------------
      watchFilesJson = builtins.toJSON checkedWatchFiles;
      runOptsJson = builtins.toJSON checkedExtraOptions;
      envForwardJson = builtins.toJSON checkedEnvForward;

      # ---- shellHook construction ---------------------------------------------
      # Order: preHook → export NCAP_PROJECT_ROOT → guarded watch_file per entry → init when autoStart → postHook
      watchFileLines = lib.optionalString (checkedWatchFiles != [ ]) "[ -n \"\${DIRENV_DIR:-}\" ] && watch_file ${
        lib.concatMapStringsSep " " lib.escapeShellArg checkedWatchFiles
      }";

      # The init call must not abort shell entry on failure; wrap with warning.
      initHook = lib.optionalString checkedAutoStart ''
        if ! ncap-ctl init; then
          echo "ncap-ctl: init failed (run \`ncap-ctl init\` to retry; wrapped commands will hint on connect)" >&2
        fi
      '';

      shellHookFragments = lib.concatStringsSep "\n" (
        lib.filter (s: s != "") [
          checkedPreShellHook
          ''export NCAP_PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"''
          watchFileLines
          initHook
          checkedPostShellHook
        ]
      );
    in
    builtins.seq checkAll (
      pkgs.mkShellNoCC {
        name = "nix-capsule-shell";

        NCAP_PROJECT = checkedProject;
        NCAP_CONTAINER = checkedContainerName;
        NCAP_SOCKET = checkedSocketPath;
        NCAP_CACHE_DIR = checkedCacheDir;
        NCAP_LOG_DIR = checkedLogDir;
        NCAP_IMAGE = checkedImage;
        NCAP_DEVSHELL = checkedDevShell;
        NCAP_WATCH_FILES = watchFilesJson;
        NCAP_RUN_OPTS = runOptsJson;
        NCAP_ENV_FORWARD = envForwardJson;
        NCAP_TIMEOUT = toString checkedTimeout;
        NCAP_HARDEN = if checkedHarden then "true" else "false";
        NCAP_RUNTIME = checkedRuntime;
        NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
        NCAP_NIX = "${pkgs.nix}/bin/nix";
        NCAP_BASH = "${pkgs.bash}/bin/bash";

        packages = [ pkgs.ncap ] ++ wrapperBins;

        shellHook = shellHookFragments;
      }
    );
}
