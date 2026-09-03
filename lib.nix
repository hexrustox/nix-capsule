{ pkgs }:
let
  lib = pkgs.lib;

  # Sanitize a basename: non-alphanumeric runs collapse to single '-', edges stripped.
  # Returns null when nothing survives.
  sanitize =
    basename:
    let
      chars = lib.stringToCharacters basename;
      isAlnum = c: builtins.match "[A-Za-z0-9]" c != null;
      folded = lib.foldl' (
        acc: ch:
        if isAlnum ch then
          if acc.run && acc.out != "" then
            {
              out = acc.out + "-" + ch;
              run = false;
            }
          else
            {
              out = acc.out + ch;
              run = false;
            }
        else
          {
            out = acc.out;
            run = true;
          }
      ) { out = ""; run = false; } chars;
      # folded.out already has no leading/trailing '-', due to run logic.
      out = folded.out;
    in
    if out == "" then null else out;

  # Normalize devShell URI: bare names get ".#" prefixed, full URIs pass through.
  normalizeDevShell =
    devShell:
    if lib.hasPrefix "." devShell || lib.hasPrefix "/" devShell then devShell
    else if lib.hasInfix ":" devShell || lib.hasInfix "#" devShell then devShell
    else ".#" + devShell;

  # Validate watchFiles entries: plain strings only, no absolute, no "..".
  validateWatchFiles =
    files:
    let
      checkOne = f:
        if builtins.isPath f then
          throw "watchFiles entry `${toString f}` is a Nix path; use a plain string (a path would be copied into the store)"
        else if !builtins.isString f then
          throw "watchFiles entry must be a string, got ${builtins.typeOf f}"
        else if lib.hasPrefix "/" f then
          throw "watchFiles entry `${f}` is absolute; watchFiles must be project-root-relative and not absolute"
        else if lib.hasInfix ".." f then
          throw "watchFiles entry `${f}` contains `..`; watchFiles must be project-root-relative and not contain `..`"
        else
          f;
    in
    map checkOne files;

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
      preShellHook ? "",
      postShellHook ? "",
      autoStart ? true,
      runtime ? "podman",
    }:
    let
      # ---- validation ---------------------------------------------------------
      validatedWatchFiles = validateWatchFiles watchFiles;

      normalizedDevShell = normalizeDevShell devShell;

      # ---- project derivation -------------------------------------------------
      # When project is not set, derive from PWD basename (impure). This mirrors
      # the Rust derivation (sanitize basename, error if empty). In pure eval
      # where PWD is empty, we require an explicit `project` rather than
      # falling back to the store path (`toString ./.`) which would be misleading.
      projectName =
        if project != null then
          project
        else
          let
            pwd = builtins.getEnv "PWD";
            base = if pwd != "" then builtins.baseNameOf pwd else throw "cannot derive a project name from root `${toString ./.}`; set `project`";
            sanitized = sanitize base;
          in
          if sanitized == null then
            throw "cannot derive a project name from root `${base}`; set `project`"
          else
            sanitized;

      # ---- derived names/paths ------------------------------------------------
      containerDerived = "ncap-${projectName}";
      containerFinal = if containerName != null then containerName else containerDerived;

      # XDG fallback rule (mirrors ctl's paths.rs):
      # - socket: $XDG_RUNTIME_DIR/nix-capsule/<project>/ncap.sock (dir 0700),
      #   fallback $TMPDIR/nix-capsule-<uid>/nix-capsule/<project>/ncap.sock
      # - cache:  $XDG_CACHE_HOME/nix-capsule/<project> else $HOME/.cache/nix-capsule/<project>
      # - logs:   $XDG_STATE_HOME/nix-capsule/<project>/logs else $HOME/.local/state/nix-capsule/<project>/logs
      # At Nix eval we capture the evaluating shell's XDG/TMPDIR/HOME/UID (impure)
      # so the derived path is keyed by project and the fallback logic is visible
      # in the Nix code; the runtime ctl re-derives with the user's actual env
      # if NCAP_* is unset, but mkShell now sets them for eval-level checks.
      xdgRuntimeDir = builtins.getEnv "XDG_RUNTIME_DIR";
      tmpdir = let t = builtins.getEnv "TMPDIR"; in if t != "" then t else "/tmp";
      uid = let u = builtins.getEnv "UID"; in if u != "" then u else "1000";
      xdgCacheHome = builtins.getEnv "XDG_CACHE_HOME";
      xdgStateHome = builtins.getEnv "XDG_STATE_HOME";
      home = builtins.getEnv "HOME";
      runtimeDir = if xdgRuntimeDir != "" then "${xdgRuntimeDir}/nix-capsule/${projectName}" else "${tmpdir}/nix-capsule-${uid}/nix-capsule/${projectName}";
      socketDerived = "${runtimeDir}/ncap.sock";
      socketFinal = if socketPath != null then socketPath else socketDerived;
      cacheDerived =
        if xdgCacheHome != "" then "${xdgCacheHome}/nix-capsule/${projectName}"
        else if home != "" then "${home}/.cache/nix-capsule/${projectName}"
        else "/tmp/nix-capsule-${uid}/cache/${projectName}";
      logDerived =
        if xdgStateHome != "" then "${xdgStateHome}/nix-capsule/${projectName}/logs"
        else if home != "" then "${home}/.local/state/nix-capsule/${projectName}/logs"
        else "/tmp/nix-capsule-${uid}/state/${projectName}/logs";

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
        else if builtins.isAttrs elem then
          let
            cwdVal = elem.cwd or null;
            _cwdCheck = if cwdVal != null && builtins.isPath cwdVal then
              throw "wrapper `cwd` for `${elem.name or "unknown"}` is a Nix path; use a plain string (a path would be copied into the store)"
            else if cwdVal != null && !builtins.isString cwdVal then
              throw "wrapper `cwd` for `${elem.name or "unknown"}` must be a string, got ${builtins.typeOf cwdVal}"
            else null;
          in
          builtins.seq _cwdCheck {
            name = elem.name or (throw "wrapper attrset missing required `name`");
            command = elem.command or elem.name;
            env = elem.env or [ ];
            cwd = cwdVal;
          }
        else
          throw "wrapper entry must be a string or attrset, got ${builtins.typeOf elem}"
      ) wrappers;

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
      watchFilesJson = builtins.toJSON validatedWatchFiles;
      runOptsJson = builtins.toJSON extraOptions;
      envForwardJson = builtins.toJSON envForward;

      # ---- shellHook construction ---------------------------------------------
      # Order: preHook → export NCAP_PROJECT_ROOT → guarded watch_file per entry → init when autoStart → postHook
      watchFileLines = lib.concatMapStringsSep "\n" (
        f: "[ -n \"\${DIRENV_DIR:-}\" ] && watch_file ${lib.escapeShellArg f}"
      ) validatedWatchFiles;

      # The init call must not abort shell entry on failure; wrap with warning.
      initHook = lib.optionalString autoStart ''
        if ! ncap-ctl init; then
          echo "ncap-ctl: init failed (run \`ncap-ctl init\` to retry; wrapped commands will hint on connect)" >&2
        fi
      '';

      shellHookFragments = lib.concatStringsSep "\n" (
        lib.filter (s: s != "") [
          preShellHook
          ''export NCAP_PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"''
          watchFileLines
          initHook
          postShellHook
        ]
      );

      # Need to handle empty watchFileLines -> don't emit blank line confusion.
      # Already filtered.

    in
    pkgs.mkShellNoCC {
      name = "nix-capsule-shell";

      packages = [ pkgs.ncap ] ++ wrapperBins;

      # ----- NCAP_* contract --------------------------------------------------
      NCAP_PROJECT = projectName;
      NCAP_IMAGE = image;
      NCAP_DEVSHELL = normalizedDevShell;
      NCAP_WATCH_FILES = watchFilesJson;
      NCAP_RUN_OPTS = runOptsJson;
      NCAP_ENV_FORWARD = envForwardJson;
      NCAP_TIMEOUT = toString timeout;
      NCAP_HARDEN = if harden then "true" else "false";
      NCAP_RUNTIME = runtime;
      NCAP_CONTAINER = containerFinal;
      NCAP_SOCKET = socketFinal;
      NCAP_CACHE_DIR = cacheDerived;
      NCAP_LOG_DIR = logDerived;
      NCAP_SERVER = "${pkgs.ncap}/bin/ncap-server";
      NCAP_NIX = "${pkgs.nix}/bin/nix";
      NCAP_BASH = "${pkgs.bash}/bin/bash";

      shellHook = shellHookFragments;
    };

  app = {
    type = "app";
    program = "${pkgs.ncap}/bin/ncap-ctl";
    meta.description = "nix-capsule lifecycle";
  };
}
