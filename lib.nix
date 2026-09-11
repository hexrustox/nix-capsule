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
      "Nix path `${toString v}`"
    else
      "${builtins.typeOf v} `${builtins.toString v}`";

  throwOpt = opt: expected: v: throw "option `${opt}`: expected ${expected}, got ${showReceived v}";

  throwPath =
    opt: expected: v:
    throw "option `${opt}`: expected ${expected}, got Nix path `${toString v}` (paths must be plain strings; a Nix path would be copied into the store)";

  # ---- scalar checks (return the value on success) ---------------------------
  checkString =
    opt: v:
    if builtins.isPath v then
      throwPath opt "string" v
    else if builtins.isString v then
      v
    else
      throwOpt opt "string" v;

  checkNullOrString =
    opt: v:
    if v == null then
      null
    else if builtins.isPath v then
      throwPath opt "null or string" v
    else if builtins.isString v then
      v
    else
      throwOpt opt "null or string" v;

  checkBool =
    opt: v:
    if builtins.isPath v then
      throwPath opt "bool" v
    else if builtins.isBool v then
      v
    else
      throwOpt opt "bool" v;

  checkTimeout =
    opt: v:
    if builtins.isPath v then
      throwPath opt "positive integer" v
    else if !builtins.isInt v then
      throwOpt opt "positive integer" v
    else if v <= 0 then
      throwOpt opt "positive integer" v
    else
      v;

  checkRuntime =
    opt: v:
    if builtins.isPath v then
      throwPath opt "one of `podman`, `docker`" v
    else if !builtins.isString v then
      throwOpt opt "one of `podman`, `docker`" v
    else if v == "podman" || v == "docker" then
      v
    else
      throw "option `${opt}`: expected one of `podman`, `docker`, got string `\"${v}\"`";

  # ---- list-of-strings check (returns the list on success) -------------------
  checkStringList =
    opt: v:
    if builtins.isPath v then
      throwPath opt "list of strings" v
    else if !builtins.isList v then
      throwOpt opt "list of strings" v
    else
      map (
        e:
        if builtins.isPath e then
          throwPath opt "list of strings" e
        else if builtins.isString e then
          e
        else
          throw "option `${opt}`: expected list of strings, got entry ${showReceived e}"
      ) v;

  # ---- wrappers check --------------------------------------------------------
  checkWrappers =
    opt: v:
    if builtins.isPath v then
      throwPath opt "list of strings or attrsets" v
    else if !builtins.isList v then
      throwOpt opt "list of strings or attrsets" v
    else
      map (
        elem:
        if builtins.isPath elem then
          throwPath opt "string or attrset" elem
        else if builtins.isString elem then
          elem
        else if builtins.isAttrs elem then
          let
            hasName = elem ? name;
            nameRaw = if hasName then elem.name else throw "option `${opt}`: wrapper attrset missing required `name`";
            name =
              if builtins.isPath nameRaw then
                throwPath opt "wrapper `name` string" nameRaw
              else if builtins.isString nameRaw then
                nameRaw
              else
                throw "option `${opt}`: expected wrapper `name` to be string, got ${showReceived nameRaw}";
            commandRaw = if elem ? command then elem.command else name;
            command =
              if builtins.isPath commandRaw then
                throwPath opt "wrapper `command` string" commandRaw
              else if builtins.isString commandRaw then
                commandRaw
              else
                throw "option `${opt}`: expected wrapper `command` to be string, got ${showReceived commandRaw}";
            envRaw = if elem ? env then elem.env else [ ];
            env =
              if builtins.isPath envRaw then
                throwPath opt "wrapper `env` list of strings" envRaw
              else if !builtins.isList envRaw then
                throw "option `${opt}`: expected wrapper `env` to be list of strings, got ${showReceived envRaw}"
              else
                map (
                  e:
                  if builtins.isPath e then
                    throwPath opt "wrapper `env` list of strings" e
                  else if builtins.isString e then
                    e
                  else
                    throw "option `${opt}`: expected wrapper `env` to be list of strings, got entry ${showReceived e}"
                ) envRaw;
            cwdRaw = if elem ? cwd then elem.cwd else null;
            cwd =
              if cwdRaw == null then
                null
              else if builtins.isPath cwdRaw then
                throwPath opt "wrapper `cwd` null or string" cwdRaw
              else if builtins.isString cwdRaw then
                cwdRaw
              else
                throw "option `${opt}`: expected wrapper `cwd` to be null or string, got ${showReceived cwdRaw}";
          in
          builtins.deepSeq [ name command env cwd ] elem
        else
          throw "option `${opt}`: expected string or attrset, got ${showReceived elem}"
      ) v;

  # Normalize devShell URI: bare names get ".#" prefixed, full URIs pass through.
  normalizeDevShell =
    devShell:
    if lib.hasPrefix "." devShell || lib.hasPrefix "/" devShell then devShell
    else if lib.hasInfix ":" devShell || lib.hasInfix "#" devShell then devShell
    else ".#" + devShell;

in
{
  mkShell =
    {
      project ? null,
      image ? "alpine:latest",
      devShell ? "container",
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
      checkedTimeout = checkTimeout "timeout" timeout;
      checkedSocketPath = checkNullOrString "socketPath" socketPath;
      checkedContainerName = checkNullOrString "containerName" containerName;
      checkedCacheDir = checkNullOrString "cacheDir" cacheDir;
      checkedLogDir = checkNullOrString "logDir" logDir;
      checkedPreShellHook = checkString "preShellHook" preShellHook;
      checkedPostShellHook = checkString "postShellHook" postShellHook;
      checkedAutoStart = checkBool "autoStart" autoStart;
      checkedRuntime = checkRuntime "runtime" runtime;

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

      normalizedDevShell = normalizeDevShell checkedDevShell;

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
      watchFileLines = lib.concatMapStringsSep "\n" (
        f: "[ -n \"\${DIRENV_DIR:-}\" ] && watch_file ${lib.escapeShellArg f}"
      ) checkedWatchFiles;

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

      # Need to handle empty watchFileLines -> don't emit blank line confusion.
      # Already filtered.

      # ---- NCAP_* contract (spec/ctl.md) --------------------------------------
      # A null option leaves its NCAP_* unset (no eval-time derivation);
      # Ctl derives per contract. Ctl never sees an overridden value.
      nullableEnv =
        lib.optionalAttrs (checkedProject != null) { NCAP_PROJECT = checkedProject; }
        // lib.optionalAttrs (checkedContainerName != null) { NCAP_CONTAINER = checkedContainerName; }
        // lib.optionalAttrs (checkedSocketPath != null) { NCAP_SOCKET = checkedSocketPath; }
        // lib.optionalAttrs (checkedCacheDir != null) { NCAP_CACHE_DIR = checkedCacheDir; }
        // lib.optionalAttrs (checkedLogDir != null) { NCAP_LOG_DIR = checkedLogDir; };

      baseEnv = {
        NCAP_IMAGE = checkedImage;
        NCAP_DEVSHELL = normalizedDevShell;
        NCAP_WATCH_FILES = watchFilesJson;
        NCAP_RUN_OPTS = runOptsJson;
        NCAP_ENV_FORWARD = envForwardJson;
        NCAP_TIMEOUT = toString checkedTimeout;
        NCAP_HARDEN = if checkedHarden then "true" else "false";
        NCAP_RUNTIME = checkedRuntime;
        NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
        NCAP_NIX = "${pkgs.nix}/bin/nix";
        NCAP_BASH = "${pkgs.bash}/bin/bash";
      };

    in
    builtins.seq checkAll (
      pkgs.mkShellNoCC (
        {
          name = "nix-capsule-shell";

          packages = [ pkgs.ncap ] ++ wrapperBins;

          shellHook = shellHookFragments;
        }
        // baseEnv
        // nullableEnv
      )
    );

  app = {
    type = "app";
    program = "${pkgs.ncap}/bin/ncap-ctl";
    meta.description = "nix-capsule lifecycle";
  };
}
