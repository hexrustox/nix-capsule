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

`mkShell` wraps `pkgs.mkShellNoCC` (shell name `nix-capsule-shell`) and produces:

- `ncap` and `ncap-ctl` on PATH (plus the wrapper bins below, and any
  consumer-supplied `packages`),
- one wrapper bin per `wrappers` entry, shadowing real binaries on
  PATH (§ Wrappers),
- all configuration exported as `NCAP_*` env vars (the contract is
  spec/ctl.md § NCAP_* contract),
- the shellHook (§ shellHook).

The container shell is an ordinary devshell defined by the consumer — any
attr, any name.

Additionally the flake exposes `devShellGuard`: a shell fragment that refuses
to run outside a container (checked via the container-runtime marker files).
It is for the Container shell's own hook, not the Host shell.

## Options

An empty-string option leaves its `NCAP_*` empty in `mkShell`; the Host
shell fills it via `setup-env` per spec/ctl.md § NCAP_* contract. The
flake side makes no guarantee about the derived value.

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `project` | string | `""` | Sets `NCAP_PROJECT`. |
| `image` | string | required | Sets `NCAP_IMAGE`. |
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
| `logLevel` | string | `"warning"` | Sets `NCAP_LOG_LEVEL`. |
| `preShellHook` / `postShellHook` | strings | `""` | Extra shellHook fragments, run before/after the capsule fragments. |
| `autoStart` | bool | `true` | Run `ncap-ctl init` from the shellHook. |
| `runtime` | string | `"auto"` | Sets `NCAP_RUNTIME`. |
| `packages` | list of packages | `[ ]` | Extra packages added to the Host shell's `packages`, alongside `ncap`, `ncap-ctl`, and the wrapper bins. |
| `override` | attrset | `{ }` | Attrset merged verbatim over the result of `mkShellNoCC` (`} // override`) — the escape hatch for consumers to set or replace the Host shell's attributes directly. |

### Type checks

Every option is checked when `mkShell` is evaluated, before
`mkShellNoCC` runs. A mismatch throws at eval time; the error names
the option, the expected shape, and the received type/value. No
coercions: a
value of the wrong type is an error, never silently converted.
Type checking is check-plus-render only: lists render to JSON, `timeout`
renders to string, `harden` renders to `"true"`/`"false"`; value
transformation and validation (runtime names, timeout range, devshell shape, log-level
values) is `ctl`'s job per spec/ctl.md § NCAP_* contract.

Checked shapes: `image`, `devShell`,
`runtime` strings; `watchFiles`, `envForward`, `extraOptions` lists of
strings; `wrappers` a list of strings or attrsets (§ Wrappers);
`harden`, `autoStart` bools; `timeout` integer number;
`project`, `containerName`, `socketPath`, `cacheDir`, `logDir` strings;
`preShellHook`, `postShellHook` strings; `logLevel` a string; `override` an
attrset, merged verbatim (§ Options table above) — its values are
pass-through, no shape check on entries. `packages` is
the one deliberate exception: its entries are pass-through values (typically
derivations), appended verbatim to the shell's `packages` — no shape check or
render applies. Wrapper attrsets require `name` (string);
`command` defaults to `name`; `env` is a list of strings; `cwd` is
null or string. Unknown top-level options and unknown wrapper fields
throw at eval time naming the option or field. `image` has no default —
omitting it is the evaluator's missing-argument error.

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
failure later through the Client's connect-error hint
(spec/client.md § CLI).

## Wrappers

String shorthand routes a tool name through the Client:

```nix
wrappers = [ "cargo" ]
# writes a bin `cargo` → exec ncap cargo "$@"
```

A string entry normalizes to `command = name`, `env = []`, `cwd = null`.
Attrset form exposes exactly `name`/`command`/`env`/`cwd` — `--socket`
comes from the environment (`NCAP_SOCKET`), never from a wrapper key:

| Key | Type | Default | Maps to |
| --- | --- | --- | --- |
| `name` | string | required | bin name placed on PATH |
| `command` | string | `name` | command executed inside the container |
| `env` | list of `"KEY=VALUE"` | `[ ]` | one `--env` per entry |
| `cwd` | string | `null` | `--cwd` |

Each wrapper bin runs `exec ncap` with one shell-escaped `--env` per `env`
entry, an optional shell-escaped `--cwd`, and the shell-escaped command,
passing through `"$@"`. Wrapped and bare invocations share every Client
behavior — a wrapper is just a pre-filled command line
(spec/client.md § CLI).

## Environment layering

The layering and merge rules are spec/env.md § Merge rules: the Client
merges layers 2–4 into the request's env list; the Server applies that list
over its inherited environment.

Note the split: forwarded *values* never require a restart (the Client
re-reads them each invocation). Only editing the `envForward` *list* touches
the flake — which trips the freshness hash and restarts. Incidental, not
required.

## Freshness triggers

Entering the Host shell runs `ncap-ctl init` — `nix develop` and a direnv
reload are two interchangeable triggers for the same shellHook; direnv is
optional. The hash check governs only the container Env dump —
see spec/paths.md § Freshness and the digest: fresh means no `nix
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
- An empty `watchFiles` disables freshness tracking: the env dump is
  evaluated once — the first `init`, when the Cache is missing — and then
  never again. The empty watch list digests to a constant (spec/paths.md §
  Freshness and the digest), so flake edits don't trip it; switching between
  empty and non-empty still flips the digest once. `ncap-ctl clean` forces a
  re-eval.

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
`watchFiles` content hash (spec/paths.md § Freshness and the digest). Adding
`watch_file foo.nix` to `.envrc` without `watchFiles = [ … "foo.nix" ]`
reloads direnv on `foo.nix` edits, but the hash matches, so there is no
re-eval and no restart. To watch extra files, use `watchFiles` instead.
