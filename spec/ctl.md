# Ctl (`ncap-ctl`)

Ctl is the Host shell's lifecycle brain: it evaluates and caches the
Container shell, launches and stops the Container, and reports project state.
It runs on the host; configuration arrives as `NCAP_*` env vars (§ NCAP_*
contract).

## Commands

| Command | Behavior |
| --- | --- |
| `init` | Entry point from the shellHook. Stamp guard, probe, hash-check, then start or re-eval + restart (§ init flow). |
| `start` | Stamp guard, probe: live ⇒ done ("already running"). Not live ⇒ run the Container detached, then await readiness (§ start flow). |
| `stop` | `<runtime> stop <name>` — SIGTERM to the container's init process (the Server), graceful drain. Idempotent: not running ⇒ success. A failed stop whose follow-up probe shows not-running ⇒ success. |
| `restart` | Non-fatal `stop`, then `init` (which re-ensures the Cache before starting). |
| `enter` | `<runtime> exec -it <name> <bash> -c "source <cache>/env && exec <bash>"` — interactive escape hatch, outside the protocol. Container down ⇒ error suggesting `ncap-ctl init`. |
| `status` | Container running? Socket connectable (§ Liveness)? Cache fresh/stale/missing (§ Freshness and the digest)? |
| `log` | Open the newest Server log in `$PAGER` (fallback `less -R`). Newest = highest epoch stamp. No log file ⇒ error naming the log dir. |
| `clean` | Stop the Container, remove the (stopped) container, clear this project's Cache and log contents (best-effort dir removal, stamp included), and delete the socket file + best-effort parent dir — never recursive on an explicit path that may be shared. |
| `show-options` | Print the `$VAR`-expanded contents of `NCAP_RUN_OPTS`, one arg per line. |

## Liveness

One predicate serves both the `init` liveness probe and the `start`
readiness poll — liveness and readiness are the same check at different
call sites. Live means both: `<runtime> inspect` reports `State.Running`,
and the socket is connectable. The container reports `Running` while still
sourcing the Env dump ahead of the Server's bind, so `Running` alone is
not live.

## init flow

1. Read the stamp guard (§ Stamp guard).
2. Probe liveness (§ Liveness).
   - Live and fresh ⇒ done.
   - Live and stale/missing ⇒ re-eval (§ Freshness and the digest), then a
     non-fatal `stop`, then start (§ start flow).
   - Not live ⇒ ensure the Cache (eval if stale or missing), start.

## start flow

Preconditions: the Env dump must exist in the Cache — otherwise error
suggesting `ncap-ctl init`; the socket's parent dir is created with mode
`0700` when new (an existing dir is left untouched; § XDG layout); the log dir
is created (§ XDG layout); a container with the target name that exists but
is not running is removed before launch.

The container invocation:

```
<runtime> run -d --name <container> <mounts and options> -- <image> <NCAP_BASH> \
  -c "source <cache>/env && exec <NCAP_SERVER> --socket <socket> --log-dir <log-dir> --timeout <timeout>"
```

`<NCAP_BASH>` and `<NCAP_SERVER>` are the absolute store paths from
`NCAP_BASH` and `NCAP_SERVER` — the image provides only the sandbox, so the
launch must not rely on its `PATH` for these binaries.

Detached; `exec` makes the Server the container's init process.

Readiness: after launch, poll the liveness predicate (§ Liveness) until live
(deadline: `NCAP_TIMEOUT`, which
bounds both this readiness poll and the Server's drain grace). Losing a
concurrent-start race ("name in use"): re-inspect — running ⇒ success; dead
⇒ `rm` the container and start once more. Never reaching readiness ⇒ fail
loudly with the `inspect` state.

## Mounts

Defaults first; `extraOptions` args are appended after, with `$VAR`/`${VAR}`
expanded at launch (so `$CARGO_HOME`-style mounts work). Expansion happens in
Ctl, against its own environment; no word-splitting afterwards; a referenced
unset variable is a launch error naming it (an empty value expands to
nothing). A `$` that starts neither `${NAME}` nor `$NAME` (name =
`[A-Za-z_][A-Za-z0-9_]*`) stays literal.

| Mount | Mode | Purpose |
| --- | --- | --- |
| `/nix:/nix` | ro | Host store: bash, `ncap-server`, every devshell tool. |
| socket dir → same path | rw | Shared Unix socket at the identical path. |
| `<project root>` → same path | rw | Workspace at the identical path. |
| `-w <project root>` | — | Container working directory. |
| cache dir → same path | ro | Env dump the Server sources — **read-only so the container can't poison files the host will later source**. |
| log dir → same path | rw | Server writes its logs there. |
| `<project root>/.git` → same path | ro, if it exists | Read-only git metadata for tools that read it. |
| `<project root>/<watched files>` → same path | ro, if present and `harden` | Keeps watched files immutable from inside the container (§ Harden). |

Host paths are valid inside the container only because the project root is
bind-mounted at the same absolute path; anything not mounted is invisible
inside.

## Harden

`harden = true` prepends `--cap-drop=all --security-opt=no-new-privileges`
and bind-mounts every watched file read-only (when present) over the
read-write project-root mount — a more-specific mount wins in podman/docker,
so the files stay immutable even though the parent directory is writable.
This prevents a contained process from rewriting files whose change triggers
host-side re-evaluation (`nix print-dev-env` on the host, the shellHook
included) and re-sourcing inside the container. `flake.lock` edits and
`nix flake update` must then happen on the host; accepted tradeoff, like
`.git` being read-only. Capability drops occasionally break dev tooling, so
hardening stays opt-in.

## Runtime adapter

`NCAP_RUNTIME` names the OCI runtime: `podman` or `docker`
(any other value ⇒ error naming `NCAP_RUNTIME`).
Both runtimes speak the same argument surface; state probes
use Go-template `inspect` (`State.Running`, the JSON `State` for failure
reports). Rootless operation is the assumption.

Linux only — same-path bind mounts, read-only `/nix`, and shared Unix
sockets rule out macOS (podman-machine's VM breaks path identity).

## NCAP_* contract

| Var | Derive | Description |
| --- | --- | --- |
| `NCAP_PROJECT` | from `NCAP_PROJECT_ROOT` (§ Project name) | Project name. Set by `project`. |
| `NCAP_PROJECT_ROOT` | `-` | Project root — the git toplevel of the consumer's checkout, falling back to the current directory outside a git repo; anchors the project name, the workspace mount, and the workdir of executed commands. No option: shellHook sets it. |
| `NCAP_SOCKET` | from the project name (§ XDG layout) | Socket path. Set by `socketPath`. |
| `NCAP_CACHE_DIR` | from the project name (§ XDG layout) | Cache dir. Set by `cacheDir`. |
| `NCAP_LOG_DIR` | from the project name (§ XDG layout) | Log dir. Set by `logDir`. |
| `NCAP_CONTAINER` | `ncap-<project>` (§ Project name) | Container name. Set by `containerName`. |
| `NCAP_IMAGE` | `-` | OCI image; provides only the kernel/userland sandbox. Set by `image`. |
| `NCAP_RUNTIME` | `-` | OCI runtime (§ Runtime adapter). Set by `runtime`. |
| `NCAP_DEVSHELL` | `-` | Complete flake URI of the Container shell; no bare-name `.#` prefixing is performed. Set by `devShell`. |
| `NCAP_RUN_OPTS` | `-` | JSON array of extra runtime args, appended after the default mounts (§ Mounts). Set by `extraOptions`. |
| `NCAP_WATCH_FILES` | `-` | JSON array of project-root-relative watched files hashed for freshness (§ Freshness and the digest); also emitted as direnv `watch_file` calls (spec/flake-api.md § Freshness triggers). Set by `watchFiles`. |
| `NCAP_ENV_FORWARD` | `-` | JSON array of forwarded variable names (validated by the Client only, not by `ctl`; additionally consumed by the Client). Set by `envForward`. |
| `NCAP_SERVER` | `-` | Store path of `ncap-server`. No option: pkgs-provided. |
| `NCAP_NIX` | `-` | Store path of `nix`. No option: pkgs-provided. |
| `NCAP_BASH` | `-` | Store path of devshell bash. No option: pkgs-provided. |
| `NCAP_TIMEOUT` | `-` | Seconds (`0` allowed: no drain grace / immediate readiness deadline); bounds start readiness and the Server's drain grace. Set by `timeout`. |
| `NCAP_HARDEN` | `-` | `true`/`false` enables harden (§ Harden). Set by `harden`. |

Empty-string values count as unset. Every command demands the full `ctl`
set: `NCAP_PROJECT_ROOT`, the project derivation, `NCAP_CONTAINER`,
`NCAP_SOCKET`, `NCAP_CACHE_DIR`, `NCAP_LOG_DIR`, `NCAP_IMAGE`,
`NCAP_RUNTIME`, `NCAP_DEVSHELL`, `NCAP_NIX`, `NCAP_SERVER`, `NCAP_BASH`,
`NCAP_TIMEOUT`, `NCAP_WATCH_FILES`, `NCAP_RUN_OPTS`, and
`NCAP_HARDEN`. A missing var is an error naming the var, and a set
`NCAP_WATCH_FILES`/`NCAP_RUN_OPTS` that is not a JSON array of strings
is an error naming the var; a `NCAP_HARDEN` that is not `true`/`false`
is likewise an error. `NCAP_ENV_FORWARD` is validated by the Client
only.

These are env-contract demands, not per-command usage errors: in
practice `lib.nix` sets all of them, so a missing var is a broken
environment rather than a usage error.

Precedence: explicit env wins, else the ctl-derived value per `Derive`,
else an error naming the var. Ctl never overrides a set value. A missing
var whose `Derive` is `-` is an error naming it.

## XDG layout

All per-project state lives outside the project tree, keyed by project name.
An explicit `NCAP_SOCKET`, `NCAP_CACHE_DIR`, or `NCAP_LOG_DIR` wins; else the
ctl-derived value below applies:

| What | Path |
| --- | --- |
| Socket dir | `$XDG_RUNTIME_DIR/nix-capsule/<project>/` — created mode `0700` |
| Socket | `<socket dir>/ncap.sock` |
| Cache | `$XDG_CACHE_HOME/nix-capsule/<project>/`, else `$HOME/.cache/nix-capsule/<project>/` |
| Logs | `$XDG_STATE_HOME/nix-capsule/<project>/logs/`, else `$HOME/.local/state/nix-capsule/<project>/logs/` |

If `XDG_RUNTIME_DIR` is unset, the socket falls back to
`$TMPDIR/nix-capsule/<project>/` (`$TMPDIR` defaulting to
`/tmp`), dir `0700`. For the cache and log dirs, neither the XDG var nor
`HOME` being set is an error naming the missing variable — there is no
further fallback. The socket directory is per-user and mode `0700`, typically
on tmpfs — it disappears at logout/reboot, so stale sockets don't accumulate.

## Project name

The project name is the sanitized basename of the project root: every
non-ASCII-alphanumeric character joins the surrounding run into a single `-`;
leading and trailing `-` are stripped; case is preserved. An empty result is
a hard error telling you to set `project`. Explicit `NCAP_PROJECT`,
`NCAP_CONTAINER`, `NCAP_SOCKET`, `NCAP_CACHE_DIR`, and `NCAP_LOG_DIR` bypass
their ctl-derived values; an unset `NCAP_CONTAINER` is ctl-derived as
`ncap-<project>`.

## Stamp guard

The first thing `init`/`start`/`restart` do with the Cache: read
`<cache>/project`. Present and different from the current project root ⇒
error — "project name `<name>` is already keyed to root `<path>`; set
`project`" — two checkouts of one repo must never share a
socket/container/cache. Absent ⇒ write it (creating the Cache dir if
needed). The same root passes silently. `clean` clears the project-keyed
Cache and log contents (best-effort dir removal) and deletes the socket
file + best-effort parent dir, stamp included — never recursive on an
explicit path that may be shared.

## Freshness and the digest

Cache contents:

| File | Contents |
| --- | --- |
| `env` | the `nix print-dev-env` dump of the Container shell |
| `hash` | xxhash64 of the watched files, lowercase hex |
| `profile` | the nix profile `print-dev-env` writes; history pruned after each eval |
| `project` | the stamp file (absolute project-root path) |

The digest: xxhash64 (seed 0) over the concatenation of one record per
entry, entries sorted by relative path — `(relative path, NUL, exists flag,
NUL, contents-or-empty)`. Missing files contribute their absence flag, so a
file appearing or disappearing flips freshness; a mtime-only touch does not;
the relative path is part of the record, so moving a watched file flips it
too. Cached as lowercase hex, no trailing newline.

Freshness states: **missing** (no `env` in the Cache), **fresh** (`env`
present and `hash` equals the computed digest after trimming surrounding
whitespace, so a stray trailing newline still reads as fresh), **stale**
(everything else — hash differs or is unreadable).

Ensuring the Cache: fresh ⇒ nothing happens — no Nix evaluation. Stale or
missing ⇒ `nix print-dev-env --profile <cache>/profile <devshell>` runs on
the host, its stdout written to `<cache>/env`, the profile's history wiped
after each eval (a failed prune is non-fatal), and the new digest computed
and stored.

The trigger surface — shell entry, direnv reload, the no-auto-refresh
consequences — is spec/flake-api.md § Freshness triggers.
