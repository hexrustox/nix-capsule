{ pkgs }:
let
  lib = pkgs.lib;

  showReceived =
    v:
    if v == null then
      "null"
    else if builtins.isBool v then
      "boolean ${if v then "true" else "false"}"
    else if builtins.isString v then
      "string `\"${v}\"`"
    else if builtins.isPath v then
      "nix path `${toString v}`"
    else if builtins.isAttrs v then
      "attrset with keys `${builtins.concatStringsSep " " (builtins.attrNames v)}`"
    else
      "${builtins.typeOf v} `${toString v}`";

  mkErr =
    opt: expected: got:
    throw "option `${opt}`: expected ${expected}, got ${got}";

  mkEntryErr =
    opt: expected: got:
    throw "option `${opt}`: expected ${expected}, got entry ${got}";

  mkFieldErr =
    opt: field: expected: got:
    throw "option `${opt}`: expected wrapper `${field}` to be ${expected}, got ${got}";

  checkPrim =
    expected: pred: opt: v:
    if pred v then v else mkErr opt expected (showReceived v);

  checkString = checkPrim "string" builtins.isString;
  checkBool = checkPrim "bool" builtins.isBool;
  checkInt = checkPrim "integer" builtins.isInt;

  checkStringList =
    opt: v:
    if !builtins.isList v then
      mkErr opt "list of strings" (showReceived v)
    else
      map (e: if builtins.isString e then e else mkEntryErr opt "list of strings" (showReceived e)) v;

  checkFieldString =
    opt: field: v:
    if builtins.isString v then v else mkFieldErr opt field "a string" (showReceived v);

  checkFieldStringList =
    opt: field: v:
    if !builtins.isList v then
      mkFieldErr opt field "a list of strings" (showReceived v)
    else
      map (
        e:
        if builtins.isString e then
          e
        else
          mkFieldErr opt field "a list of strings" "entry ${showReceived e}"
      ) v;

  checkWrappers =
    opt: v:
    if !builtins.isList v then
      mkErr opt "list of strings or attrsets" (showReceived v)
    else
      map (
        elem:
        if builtins.isString elem then
          {
            name = elem;
            command = elem;
            env = [ ];
            cwd = null;
          }
        else if builtins.isAttrs elem then
          let
            extra = builtins.filter (
              k:
              !(builtins.elem k [
                "name"
                "command"
                "env"
                "cwd"
              ])
            ) (builtins.attrNames elem);
            name =
              if extra != [ ] then
                throw "option `${opt}`: unknown wrapper field `${builtins.head extra}`"
              else if elem ? name then
                checkFieldString opt "name" elem.name
              else
                throw "option `${opt}`: expected wrapper attrset to have a string `name`, got attrset without one";
            command = if elem ? command then checkFieldString opt "command" elem.command else name;
            env = checkFieldStringList opt "env" (elem.env or [ ]);
            cwdRaw = elem.cwd or null;
            cwd =
              if cwdRaw == null || builtins.isString cwdRaw then
                cwdRaw
              else
                mkFieldErr opt "cwd" "null or a string" (showReceived cwdRaw);
          in
          builtins.deepSeq [ name command env cwd ] {
            inherit
              name
              command
              env
              cwd
              ;
          }
        else
          mkErr opt "a string or attrset" (showReceived elem)
      ) v;

  checkOverride = opt: v: if builtins.isAttrs v then v else mkErr opt "attrset" (showReceived v);

  checkers = {
    project = checkString;
    image = checkString;
    devShell = checkString;
    watchFiles = checkStringList;
    envForward = checkStringList;
    wrappers = checkWrappers;
    extraOptions = checkStringList;
    harden = checkBool;
    timeout = checkInt;
    socketPath = checkString;
    containerName = checkString;
    cacheDir = checkString;
    logDir = checkString;
    logLevel = checkString;
    preShellHook = checkString;
    postShellHook = checkString;
    autoStart = checkBool;
    runtime = checkString;
    packages = _: v: v;
    override = checkOverride;
  };

  defaults = {
    project = "";
    devShell = ".#container";
    watchFiles = [
      "flake.nix"
      "flake.lock"
    ];
    envForward = [ ];
    wrappers = [ ];
    extraOptions = [ ];
    harden = false;
    timeout = 10;
    socketPath = "";
    containerName = "";
    cacheDir = "";
    logDir = "";
    logLevel = "warning";
    preShellHook = "";
    postShellHook = "";
    autoStart = true;
    runtime = "auto";
    packages = [ ];
    override = { };
  };
in
{
  mkShell =
    {
      image,
      ...
    }@args:
    let
      unknownOpts = builtins.filter (opt: !(builtins.hasAttr opt checkers)) (builtins.attrNames args);
      checked = builtins.mapAttrs (opt: check: check opt (args.${opt} or defaults.${opt})) checkers;

      checkAll =
        if unknownOpts != [ ] then
          throw "option `${builtins.head unknownOpts}`: unknown option"
        else
          builtins.deepSeq checked true;

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
        if ! source <(ncap-ctl setup-env); then
          echo "ncap-ctl: setup-env failed (run \`ncap-ctl setup-env\` to retry)" >&2
        fi
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
    builtins.seq checkAll (
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
