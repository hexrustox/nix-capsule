# Flake-facing API

nix-capsule runs a Nix devshell inside an OCI container while the user stays
in their host shell (see `CONTEXT.md` for vocabulary). It splits a devshell
in two:

- a **Host shell** (e.g. `devShells.default` in the consumer's flake): the
  Client `ncap`, Ctl `ncap-ctl`, and Wrapper scripts routing tool names
  through the Client.
- a **Container shell** (a second devshell attr, e.g. `devShells.container`):
  the real toolchain, never entered directly.

`nix develop` drops the user into the Host shell; the shellHook starts the
container. The Container is a dumb sandbox — the host's `/nix` store is
mounted read-only and the Container shell is pre-evaluated on the host (`nix
print-dev-env`) into the cached Env dump that the Server sources; no Nix
evaluation runs inside the container.

The `nix-capsule` flake exposes to a consuming flake:

- an **overlay** providing the `ncap` package (Client, Server, and Ctl
  binaries),
- **`mkShell`** — builds the Host shell around a referenced Container shell,

A minimal consuming flake needs nothing else:

```nix
{
  inputs.nix-capsule.url = "…";

  outputs = { self, nixpkgs, nix-capsule }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ nix-capsule.overlays.default ];
      };
      inherit (nix-capsule.lib { inherit pkgs; }) mkShell;
    in
    {
      devShells.${system} = {
        default = mkShell {
          wrappers = [ "cargo" ];
          envForward = [ "CARGO_HOME" ];
          # …
        };
        container = pkgs.mkShell { packages = [ /* real toolchain */ ]; };
      };
    };
}
```

Shell names used in examples (`default`, `container`) are placeholders — the
consumer's flake names both shells; only the linkage between them matters.

## mkShell

`mkShell` wraps `pkgs.mkShellNoCC` and produces:

- `ncap` and `ncap-ctl` on PATH,
- one `writeShellScriptBin` per `wrappers` entry, shadowing real binaries on
  PATH (§ Wrappers),
- all configuration exported as `NCAP_*` env vars (the contract is
  spec/ctl.md § NCAP_* contract),
- the shellHook (§ shellHook).

The container shell is an ordinary devshell defined by the consumer — any
attr, any name.

## Options

An empty-string option leaves its `NCAP_*` empty in `mkShell`; the Host
shell fills it via `setup-env` per spec/ctl.md § NCAP_* contract. The
flake side makes no guarantee about the derived value.

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `project` | string | `""` | Sets `NCAP_PROJECT`. |
| `image` | string | `"alpine:latest"` | Sets `NCAP_IMAGE`. |
| `devShell` | string | `".#container"` | Sets `NCAP_DEVSHELL`. |
| `watchFiles` | list of strings | `[ "flake.nix" "flake.lock" ]` | Sets `NCAP_WATCH_FILES`. |
| `envForward` | list of strings | `[ ]` | Sets `NCAP_ENV_FORWARD`. |
| `wrappers` | list | `[ ]` | Host PATH shims routing tool names through the Client (§ Wrappers). |
| `extraOptions` | list of strings | `[ ]` | Sets `NCAP_RUN_OPTS`. |
| `harden` | bool | `false` | Sets `NCAP_HARDEN`. |
| `timeout` | int | `10` | Sets `NCAP_TIMEOUT`. |
| `socketPath` | string | `""` | Sets `NCAP_SOCKET`. |
| `containerName` | string | `""` | Sets `NCAP_CONTAINER`. |
| `cacheDir` | string | `""` | Sets `NCAP_CACHE_DIR`. |
| `logDir` | string | `""` | Sets `NCAP_LOG_DIR`. |
| `preShellHook` / `postShellHook` | strings | `""` | Extra shellHook fragments, run before/after the capsule fragments. |
| `autoStart` | bool | `true` | Run `ncap-ctl init` from the shellHook. |
| `runtime` | string | `"podman"` | Sets `NCAP_RUNTIME`. |

### Type checks

Every option is checked when `mkShell` is evaluated, before
`mkShellNoCC` runs. A mismatch throws at eval time; the error names
the option, the expected shape, and the received type/value. No
coercions: a
value of the wrong type is an error, never silently converted.
`lib.nix` performs type checks only; value transformation and
validation (runtime names, timeout range, devshell shape) is `ctl`'s job.

Checked shapes: `image`, `devShell`,
`runtime` strings; `watchFiles`, `envForward`, `extraOptions` lists of
strings; `wrappers` a list of strings or attrsets (§ Wrappers);
`harden`, `autoStart` bools; `timeout` integer number;
`project`, `containerName`, `socketPath`, `cacheDir`, `logDir` strings;
`preShellHook`, `postShellHook` strings. Wrapper attrsets require `name` (string);
`command` defaults to `name`; `env` is a list of strings; `cwd` is
null or string. Unknown top-level options and unknown wrapper fields
throw at eval time naming the option or field.

## shellHook

The shellHook runs, in order (empty fragments skipped):

1. `preShellHook`,
2. exports `NCAP_PROJECT_ROOT` (git toplevel, falling back to `pwd`),
3. sources `setup-env` output (`source <(ncap-ctl setup-env)`) resolving
   the project-scoped envs per spec/ctl.md § NCAP_* contract,
4. emits one guarded `watch_file` invocation taking every `watchFiles`
   entry as arguments (guard: `command -v watch_file` — inert outside
   direnv); an empty `watchFiles` emits no line,
5. runs `ncap-ctl init` when `autoStart`,
6. `postShellHook`.

A failing `ncap-ctl setup-env` or `ncap-ctl init` prints a warning on
stderr and does not abort shell entry — wrapped commands surface the
failure later through the Client's connect-error hint.

## Wrappers

String shorthand routes a tool name through the Client:

```nix
wrappers = [ "cargo" ]
# writes a bin `cargo` → exec ncap cargo "$@"
```

Attrset form exposes exactly `name`/`command`/`env`/`cwd` — `--socket`
comes from the environment (`NCAP_SOCKET`), never from a wrapper key:

| Key | Type | Default | Maps to |
| --- | --- | --- | --- |
| `name` | string | required | bin name placed on PATH |
| `command` | string | `name` | command executed inside the container |
| `env` | list of `"KEY=VALUE"` | `[ ]` | one `--env` per entry |
| `cwd` | string | `null` | `--cwd` |

Wrapped and bare invocations share every Client behavior — a wrapper is just
a pre-filled command line (spec/client.md § CLI).

## Environment layering

A Child inside the container sees four layers; for a given KEY, the highest
layer defining it wins:

| # | Layer | Resolved | Changes take effect |
| --- | --- | --- | --- |
| 1 | Env dump (Server's inherited env) | at init, inside the container | after re-eval + restart |
| 2 | `envForward` | by the Client, from the host env, **per request** | immediately |
| 3 | wrapper `env` | in the wrapper script | after flake edit (wrapper regen) |
| 4 | `-e KEY=VALUE` | CLI flag, per invocation | immediately |

The Client merges layers 2–4 into the request's env list; the Server applies
that list over its inherited environment.

Note the split: forwarded *values* never require a restart (the Client
re-reads them each invocation). Only editing the `envForward` *list* touches
the flake — which trips the freshness hash and restarts. Incidental, not
required.

## Freshness triggers

Entering the Host shell runs `ncap-ctl init` — `nix develop` and a direnv
reload are two interchangeable triggers for the same shellHook; direnv is
optional. The hash check governs only the container Env dump —
see spec/ctl.md § Freshness and the digest: fresh means no `nix
print-dev-env`, stale means re-eval + restart.

Sessions never auto-refresh: a user sitting in a long-lived `nix develop`
session gets no container refresh after a flake edit — edits take effect on
the next shell entry (or direnv reload). Identical to upstream `nix develop`
behavior; a host-side file watcher is deliberately out of scope.

Consequences, all deliberate:

- direnv's `watch_file` is mtime-based: touching a watched file with
  identical content still triggers a reload, but the hash matches, so the
  cost is one hash pass — no evaluation, no restart.
- Any `flake.nix` edit — even Host-shell-only options like `wrappers` or
  `envForward` — trips the hash and restarts the container. Correct,
  occasionally heavier than needed.
- Files not in `watchFiles` (e.g. locally imported `.nix` files) don't affect
  freshness — add them to `watchFiles` (under `harden`, this also grants
  them write protection inside the container).

### direnv users (optional)

direnv integration needs nothing beyond a standard `.envrc`:

```sh
use flake .
```

The shellHook's guarded `watch_file` emission adds configured `watchFiles`
entries to direnv's watch set automatically — no manual `watch_file` lines
needed. The emission is inert without direnv and additive with it — there
is no opt-out knob. Not wanting the direnv integration means not using
direnv; full manual control is `autoStart = false`.

A `watch_file` in `.envrc` only triggers a direnv reload; it does not
affect nix-capsule's cache freshness. Freshness is decided solely by the
`watchFiles` content hash (spec/ctl.md § Freshness and the digest). Adding
`watch_file foo.nix` to `.envrc` without `watchFiles = [ … "foo.nix" ]`
reloads direnv on `foo.nix` edits, but the hash matches, so there is no
re-eval and no restart. To watch extra files, use `watchFiles` instead.
