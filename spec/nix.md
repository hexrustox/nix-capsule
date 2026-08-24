# nix-capsule — flake-facing API

The capsule exposes two things to a consuming flake:

- an **overlay** providing the `ncap` package (client, server, and ctl binaries), and
- **`mkShell`** — builds the host shell around a referenced container shell.

`mkShell` wraps `pkgs.mkShellNoCC`. The container shell is an ordinary devshell defined by the consumer — any attr, any name. A minimal consuming flake needs nothing else:

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
      capsule = nix-capsule.lib { inherit pkgs; };
    in
    {
      devShells.${system} = {
        default = capsule.mkShell {
          wrappers = [ "cargo" ];
          envForward = [ "CARGO_HOME" ];
          # …
        };
        container = pkgs.mkShell { packages = [ /* real toolchain */ ]; };
      };
    };
}
```

## Options

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `project` | string | `basename $PROJECT_ROOT`, sanitized | Namespace for socket, container, cache, and logs. |
| `image` | string | `"alpine:latest"` | OCI image; provides only the kernel/userland sandbox. |
| `devShell` | string | `"container"` | Flake URI of the container shell. Bare names get `.#` prefixed; full URIs pass through. |
| `watchFiles` | list of paths | `[ "flake.nix" "flake.lock" ]` | Inputs hashed for staleness detection; also emitted as direnv `watch_file` calls. |
| `envForward` | list of strings | `[ ]` | Env var names the client resolves from the host env, per request. |
| `wrappers` | list | `[ ]` | Host PATH shims routing tool names through the client (schema below). |
| `extraOptions` | list of strings | `[ ]` | Extra runtime args (mounts, env passes), appended after the default mounts; `$VAR`/`${VAR}` expanded at launch. |
| `harden` | bool | `false` | Add `--cap-drop=all --security-opt=no-new-privileges`. |
| `timeout` | seconds | `10` | Grace period for draining connections when the container stops. |
| `socketPath` | path | derived | Override the Unix socket path. |
| `containerName` | string | derived | Override the container name. |
| `preShellHook` / `postShellHook` | strings | `""` | Extra shellHook fragments, run before/after capsule init. |
| `autoStart` | bool | `true` | Run `ncap-ctl init` from the shellHook. |

What `mkShell` produces:

- `ncap` and `ncap-ctl` on PATH,
- one `writeShellScriptBin` per `wrappers` entry, shadowing real binaries on PATH,
- all configuration exported as `NCAP_*` env vars (contract in `ctl.md`),
- a shellHook that:
  1. runs `preShellHook`,
  2. exports `PROJECT_ROOT` (git toplevel, falling back to `pwd`),
  3. emits a guarded `watch_file <f>` for every `watchFiles` entry (no-op outside direnv),
  4. runs `ncap-ctl init` when `autoStart`,
  5. runs `postShellHook`.

## Derived names and paths

All capsule state lives outside the project tree, keyed by `project`:

| What | Path / name |
| --- | --- |
| Socket | `$XDG_RUNTIME_DIR/nix-capsule/<project>/ncap.sock` (dir `0700`) |
| Container | `ncap-<project>` |
| Cache | `$XDG_CACHE_HOME/nix-capsule/<project>/` |
| Logs | `$XDG_STATE_HOME/nix-capsule/<project>/logs/` |

If `XDG_RUNTIME_DIR` is unset, the socket falls back to `$TMPDIR/nix-capsule-<uid>/nix-capsule/<project>/` (dir `0700`).

`project` defaults to the sanitized basename of `PROJECT_ROOT` (non-alphanumeric runs collapse to `-`). `socketPath` and `containerName` override their derived values; everything else derives from `project`.

**Collision guard:** the cache holds a stamp file recording the absolute `PROJECT_ROOT` that created it. If `ncap-ctl` finds the same project name owned by a different checkout, it errors with a hint to set `project` — two checkouts of one repo must never share a socket/container/cache.

Cache contents:

| File | Contents |
| --- | --- |
| `env` | the `nix print-dev-env` dump of the container shell |
| `hash` | xxhash64 of `watchFiles`, hex |
| `profile` | the nix profile `print-dev-env` writes; history pruned after each eval |
| `project` | the stamp file (absolute `PROJECT_ROOT`) |

## Staleness

Entering the host shell (or a direnv reload) runs `ncap-ctl init`, which hashes `watchFiles` with **xxhash64** and compares against the cached `hash`:

- **Match** → start the container if it isn't running. No Nix evaluation.
- **Mismatch** → re-run `nix print-dev-env` into the cache (on the host), then restart the container so it picks up the new environment.

Consequences, all deliberate:

- direnv's `watch_file` is mtime-based: touching a watched file with identical content still triggers a reload, but the hash matches, so the cost is one hash pass — no evaluation, no restart.
- Any `flake.nix` edit — even host-shell-only options like `wrappers` or `envForward` — trips the hash and restarts the container. Correct, occasionally heavier than needed.
- Files not in `watchFiles` (e.g. locally imported `.nix` files) don't participate in staleness — add them to `watchFiles`.

direnv integration needs nothing beyond a standard `.envrc`:

```sh
watch_file flake.nix flake.lock
use flake .
```

The shellHook's guarded `watch_file` emission adds configured entries to direnv's watch set automatically.

## Wrappers

String shorthand routes a tool name through the client:

```nix
wrappers = [ "cargo" ]
# writes a bin `cargo` → exec ncap cargo "$@"
```

Attrset form mirrors client CLI flags one-to-one — the wrapper surface grows with the client's flags:

| Key | Type | Default | Maps to |
| --- | --- | --- | --- |
| `name` | string | required | bin name placed on PATH |
| `command` | string | `name` | command executed inside the container |
| `env` | list of `"KEY=VALUE"` | `[ ]` | one `--env` per entry |
| `cwd` | path | `null` | `--cwd` |

## Environment layering

A child inside the container sees four layers; for a given KEY, the highest layer defining it wins:

| # | Layer | Resolved | Changes take effect |
| --- | --- | --- | --- |
| 1 | devshell dump (server's inherited env) | at init, inside the container | after re-eval + restart |
| 2 | `envForward` | by the client, from the host env, **per request** | immediately |
| 3 | wrapper `env` | in the wrapper script | after flake edit (wrapper regen) |
| 4 | `-e KEY=VALUE` | CLI flag, per invocation | immediately |

The client merges layers 2–4 into the request's env list (later wins, deduplicated); the server applies that list over its inherited environment. A name in `envForward` that is unset on the host, and a `KEY` without `=VALUE`, are silently omitted.

Note the split: forwarded *values* never require a restart (the client re-reads them each invocation). Only editing the `envForward` *list* touches the flake — which trips the staleness hash and restarts. Incidental, not required.
