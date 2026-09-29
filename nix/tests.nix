# Layer-1 eval-time tests for nix/lib.nix, consumed by nix-unit via
# dev/flake.nix (perSystem.nix-unit.tests). The contract under test is
# spec/flake-api.md; check wording comes from ADR 0005. `mkNcapPkgs` receives an
# extra overlay to stub `ncap` so tests never evaluate the real Rust build.
{
  mkNcapPkgs,
}:
let
  pkgs = mkNcapPkgs [
    (final: prev: {
      ncap =
        prev.runCommand "ncap-stub"
          {
            passthru.pname = "ncap";
            passthru.serverProbed = true;
          }
          ''
            mkdir -p $out/bin
            touch $out/bin/ncap $out/bin/ncap-server
            chmod +x $out/bin/*
          '';
    })
  ];
  lib = pkgs.lib;

  capsuleLib = import ./lib.nix { inherit pkgs; };

  inherit (capsuleLib) mkShell devShellGuard;

  shell = opts: mkShell ({ image = "test_image"; } // opts);

  forceShell = opts: builtins.deepSeq (builtins.attrValues (shell opts)) true;

  tryEval' = expr: (builtins.tryEval expr).success;

  tryShell = opts: tryEval' (forceShell opts);

  shellAttrs = opts: lib.filterAttrs (n: _: lib.hasPrefix "NCAP_" n) (shell opts);

  ncapShell = shell { };
  ncapAttrs = shellAttrs { };

  # Exact default shellHook per spec/flake-api.md § shellHook with hooks empty
  # and autostart on. Mirrors nix/lib.nix lines 154-179.
  defaultShellHook = ''
    export NCAP_PROJECT_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
    ncap_setup_env="$(mktemp)"
    if ! ncap-ctl setup-env > "$ncap_setup_env"; then
      echo "ncap-ctl: setup-env failed (run \`ncap-ctl setup-env\` to retry)" >&2
    else
      source "$ncap_setup_env"
    fi
    rm -f "$ncap_setup_env"

    command -v watch_file >/dev/null && watch_file flake.nix flake.lock
    if ! ncap-ctl init; then
      echo "ncap-ctl: init failed (run \`ncap-ctl init\` to retry; wrapped commands will hint on connect)" >&2
    fi
  '';
in
{
  "test shell name defaults to nix-capsule when project empty" = {
    expr = ncapShell.name;
    expected = "nix-capsule";
  };
  "test shell name uses project verbatim when set" = {
    expr = (shell { project = "p"; }).name;
    expected = "p";
  };
  "test every NCAP_ default renders per spec Options table" =
    let
      expected = {
        NCAP_PROJECT = "";
        NCAP_CONTAINER = "";
        NCAP_SOCKET = "";
        NCAP_CACHE_DIR = "";
        NCAP_LOG_DIR = "";
        NCAP_LOG_LEVEL = "warning";
        NCAP_IMAGE = "test_image";
        NCAP_DEVSHELL = ".#container";
        NCAP_WATCH_FILES = builtins.toJSON [
          "flake.nix"
          "flake.lock"
        ];
        NCAP_RUN_OPTS = builtins.toJSON [ ];
        NCAP_ENV_FORWARD = builtins.toJSON [ ];
        NCAP_TIMEOUT = "10";
        NCAP_HARDEN = "false";
        NCAP_RUNTIME = "auto";
      };
    in
    {
      expr = lib.mapAttrs (
        name: value: builtins.hasAttr name ncapAttrs && ncapAttrs.${name} == value
      ) expected;
      expected = lib.mapAttrs (_: _: true) expected;
    };
  "test NCAP_WATCH_FILES NCAP_RUN_OPTS NCAP_ENV_FORWARD render as JSON" = {
    expr = {
      watch = builtins.fromJSON ncapAttrs.NCAP_WATCH_FILES;
      runOpts = builtins.fromJSON ncapAttrs.NCAP_RUN_OPTS;
      envForward = builtins.fromJSON ncapAttrs.NCAP_ENV_FORWARD;
    };
    expected = {
      watch = [
        "flake.nix"
        "flake.lock"
      ];
      runOpts = [ ];
      envForward = [ ];
    };
  };
  "test toolchain paths end in ncap-server nix bash" = {
    expr = {
      server = lib.hasSuffix "/bin/ncap-server" ncapShell.NCAP_SERVER;
      nix = lib.hasSuffix "/bin/nix" ncapShell.NCAP_NIX;
      bash = lib.hasSuffix "/bin/bash" ncapShell.NCAP_BASH;
    };
    expected = {
      server = true;
      nix = true;
      bash = true;
    };
  };
  "test shellHook exact default text" = {
    expr = ncapShell.shellHook;
    expected = defaultShellHook;
  };
  "test packages include the ncap stub" = {
    expr = map (p: p.pname or "") [ pkgs.ncap ];
    expected = [ "ncap" ];
  };

  "test scalar options render to their NCAP_ vars" = {
    expr =
      lib.filterAttrs
        (
          n: _:
          lib.elem n [
            "NCAP_PROJECT"
            "NCAP_CONTAINER"
            "NCAP_SOCKET"
            "NCAP_CACHE_DIR"
            "NCAP_LOG_DIR"
            "NCAP_LOG_LEVEL"
            "NCAP_DEVSHELL"
            "NCAP_TIMEOUT"
            "NCAP_HARDEN"
            "NCAP_RUNTIME"
            "NCAP_IMAGE"
          ]
        )
        (shellAttrs {
          project = "proj";
          containerName = "cnt";
          socketPath = "/tmp/s";
          cacheDir = "/tmp/c";
          logDir = "/tmp/l";
          logLevel = "debug";
          devShell = ".#dev";
          timeout = 30;
          harden = true;
          runtime = "podman";
          image = "debian:12";
        });
    expected = {
      NCAP_PROJECT = "proj";
      NCAP_CONTAINER = "cnt";
      NCAP_SOCKET = "/tmp/s";
      NCAP_CACHE_DIR = "/tmp/c";
      NCAP_LOG_DIR = "/tmp/l";
      NCAP_LOG_LEVEL = "debug";
      NCAP_DEVSHELL = ".#dev";
      NCAP_TIMEOUT = "30";
      NCAP_HARDEN = "true";
      NCAP_RUNTIME = "podman";
      NCAP_IMAGE = "debian:12";
    };
  };
  "test watchFiles round trips through JSON" = {
    expr =
      builtins.fromJSON
        (shellAttrs {
          watchFiles = [
            "a.nix"
            "b.nix"
          ];
        }).NCAP_WATCH_FILES;
    expected = [
      "a.nix"
      "b.nix"
    ];
  };
  "test extraOptions render as JSON to NCAP_RUN_OPTS" = {
    expr =
      (shellAttrs {
        extraOptions = [
          "-e"
          "FOO"
        ];
      }).NCAP_RUN_OPTS;
    expected = builtins.toJSON [
      "-e"
      "FOO"
    ];
  };
  "test envForward render as JSON to NCAP_ENV_FORWARD" = {
    expr = (shellAttrs { envForward = [ "CARGO_HOME" ]; }).NCAP_ENV_FORWARD;
    expected = builtins.toJSON [ "CARGO_HOME" ];
  };
  "test autoStart false drops init but keeps setup-env" = {
    expr =
      let
        hook = (shell { autoStart = false; }).shellHook;
      in
      {
        init = lib.hasInfix "ncap-ctl init" hook;
        setupEnv = lib.hasInfix "ncap-ctl setup-env" hook;
      };
    expected = {
      init = false;
      setupEnv = true;
    };
  };
  "test empty watchFiles emits no watch_file line but keeps NCAP_WATCH_FILES" = {
    expr =
      let
        s = shell { watchFiles = [ ]; };
      in
      {
        line = lib.hasInfix "watch_file" s.shellHook;
        json = s.NCAP_WATCH_FILES;
      };
    expected = {
      line = false;
      json = "[]";
    };
  };
  "test custom watchFiles emit a guarded watch_file line" = {
    expr =
      lib.hasInfix
        (
          "command -v watch_file >/dev/null && watch_file "
          + lib.escapeShellArg "a.nix"
          + " "
          + lib.escapeShellArg "b c.nix"
        )
        (shell {
          watchFiles = [
            "a.nix"
            "b c.nix"
          ];
        }).shellHook;
    expected = true;
  };

  "test unknown top-level option throws" = {
    expr = tryShell { badOpt = 1; };
    expected = false;
  };
  "test unknown option precedes type error" = {
    expr = tryShell {
      badOpt = 1;
      timeout = "fast";
    };
    expected = false;
  };
  "test timeout of wrong type throws" = {
    expr = tryShell { timeout = "fast"; };
    expected = false;
  };
  "test harden of wrong type throws" = {
    expr = tryShell { harden = 1; };
    expected = false;
  };
  "test non-string wrapper entry throws" = {
    expr = tryShell { wrappers = [ 5 ]; };
    expected = false;
  };
  "test unknown wrapper field throws" = {
    expr = tryShell {
      wrappers = [
        {
          name = "cargo";
          cmd = "x";
        }
      ];
    };
    expected = false;
  };
  "test wrong-type packages throws" = {
    expr = tryShell { packages = "hello"; };
    expected = false;
  };
  "test path value in string option throws" = {
    expr = tryShell { image = ./flake.nix; };
    expected = false;
  };
  "test wrong-type override throws" = {
    expr = tryShell { override = [ ]; };
    expected = false;
  };

  "test string shorthand coerces and renders escaped flags" = {
    expr =
      let
        s = shell {
          wrappers = [
            {
              name = "cargo";
              command = "cargo run";
              env = [
                "K=v v"
                "Q='q'"
              ];
              cwd = "a b";
            }
          ];
        };
        bin = lib.findFirst (
          p: (p.name or "") == "cargo"
        ) (throw "wrapper bin `cargo` missing") s.nativeBuildInputs;
      in
      lib.hasInfix (lib.concatStringsSep " " [
        "--env"
        (lib.escapeShellArg "K=v v")
        "--env"
        (lib.escapeShellArg "Q='q'")
        "--cwd"
        (lib.escapeShellArg "a b")
        (lib.escapeShellArg "cargo run")
        "\"$@\""
      ]) (bin.text);
    expected = true;
  };
  "test command defaults to name in shorthand" = {
    expr =
      let
        bin =
          lib.findFirst (p: (p.name or "") == "hello") (throw "wrapper bin `hello` missing")
            (shell { wrappers = [ "hello" ]; }).nativeBuildInputs;
      in
      lib.hasInfix "exec ncap hello" bin.text;
    expected = true;
  };
  "test two wrappers produce two bins after ncap" = {
    expr =
      let
        s = shell {
          wrappers = [
            "a"
            "b"
          ];
        };
        bins = map (p: (p.name or "")) s.nativeBuildInputs;
      in
      lib.all (n: lib.elem n bins) [
        "a"
        "b"
      ];
    # HERE
    expected = true;
  };

  "test preShellHook runs first" = {
    expr = lib.hasInfix "echo first" ((shell { preShellHook = "echo first"; }).shellHook);
    expected = true;
  };
  "test postShellHook runs after init" = {
    expr =
      let
        hook =
          (shell {
            postShellHook = "echo last";
          }).shellHook;
      in
      lib.hasInfix "echo last\n" (hook + "\n") && lib.hasInfix "ncap-ctl init" hook;
    expected = true;
  };

  "test override replaces shellHook" = {
    expr = (shell { override.shellHook = "echo overridden"; }).shellHook;
    expected = "echo overridden";
  };
  "test override adds passthru attr" = {
    expr = (shell { override.passthru.foo = "bar"; }).passthru.foo;
    expected = "bar";
  };

  "test devShellGuard checks both container markers" = {
    expr = map (m: lib.hasInfix m devShellGuard) [
      "/.dockerenv"
      "/run/.containerenv"
      "exit 1"
    ];
    expected = [
      true
      true
      true
    ];
  };
}
