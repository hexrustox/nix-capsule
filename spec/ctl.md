# Ctl (`ncap-ctl`)

Ctl is the Host shell's lifecycle brain: it evaluates and caches the
Container shell, launches and stops the Container, and reports project state.
It runs on the host; configuration arrives as `NCAP_*` env vars (§ NCAP_*
contract). Project identity and freshness are spec/paths.md § Project name
(identity, layout, stamp, digest); the runtime, mounts, and liveness are
spec/runtime.md § Runtime adapter (mounts, liveness, launch) — this file
references them and never restates them.

## Commands

| Command | Behavior |
| --- | --- |
| `init` | Entry point from the shellHook. Stamp guard, probe, hash-check, then start or re-eval + restart (§ init flow). |
| `start` | Stamp guard, probe: live ⇒ done. Not live ⇒ run the Container detached, then await readiness (§ start flow). |
| `stop` | `<runtime> stop <name>` — SIGTERM to the container's init process (the Server), graceful drain. Idempotent: not running ⇒ success. A failed stop whose follow-up probe shows not-running ⇒ success. |
| `restart` | Non-fatal `stop`, then `init` (which re-ensures the Cache before starting). The `stop` is runtime-only, so the first *cache* touch is still the stamp guard inside `init`. |
| `enter` | `<runtime> exec -it <name> <bash> -c "source '<cache>/env' && exec '<bash>'"` — interactive escape hatch, outside the protocol. Container down ⇒ error suggesting `ncap-ctl init`. |
| `status` | Three lines on stdout: container running (with name) or not running; socket connectable (with path) or unreachable (with path); cache fresh/stale/missing (spec/paths.md § Freshness and the digest). |
| `log` | Open the newest Server log (spec/paths.md § Server log files) in `$PAGER` (split on whitespace into program + args; fallback `less -R` when unset or blank), stdio inherited. No log file ⇒ error naming the log dir. A pager that fails to spawn or exits non-zero ⇒ failure. |
| `clean` | Stop the Container (best-effort), remove the (stopped) container, remove this project's cache files and log files (spec/paths.md § Cache contents) and delete the socket file + best-effort parent dir — never recursive on an explicit path that may be shared. |
| `show-options` | Print the `$VAR`-expanded contents of `NCAP_RUN_OPTS` (expansion per spec/runtime.md § Mounts), one arg per line. |
| `setup-env` | Resolve the five project-scoped vars and print them as `export` lines for the Host shell to source. Needs only `NCAP_PROJECT_ROOT` (§ NCAP_* contract). Never starts containers, never touches the Cache. |

Ctl exits `0` on success and non-zero on failure. A failure names the
relevant var, path, or object; some failures carry a sibling advice line
(`init` for a down container or missing cache, `project` for an identity
clash, runtime install for a missing runtime). Exact message text is style,
not behavior — only the exit status and the named-object + advice presence
are pinned here.

## Liveness

Liveness is spec/runtime.md § Liveness: one predicate (`Running` and
socket-connectable) serving both the `init` liveness probe and the `start`
readiness poll.

## Version probe

After liveness/connectability, `status` and `start` open one Version probe
Connection (spec/protocol.md § Connection lifecycle): send `RequestVersion`,
await `ServerVersion`, compare the reply against the host binaries' version
by exact string inequality — **Version skew**. Skew ⇒ one warning line on
stderr naming the Server's and the host's version and advising
`ncap-ctl restart`; the three status lines, exit status, and all other
output are unchanged. The warning never fails the command.

A Server that predates the probe rejects `RequestVersion` as an unknown tag
(`Error` and close) — the same warning prints; the stale Server is advice
identical. A probe whose socket is unreachable is skipped silently:
liveness/readiness already report that. `init` never probes — a warning
inside the shellHook's live-and-fresh "done" contradicts it; the documented
remedy is `restart` (spec/protocol.md § Guarantees — freshness tracks
watched files, not the package version, so nothing auto-heals skew).

## init flow

1. Read the stamp guard (spec/paths.md § Stamp guard).
2. Probe liveness (spec/runtime.md § Liveness).
   - Live and fresh ⇒ done.
   - Live and stale/missing ⇒ re-eval (§ Freshness and the digest), then a
     non-fatal `stop`, then start (§ start flow).
   - Not live ⇒ ensure the Cache (eval if stale or missing), start.

## start flow

Preconditions: the Env dump must exist in the Cache — otherwise error
suggesting `ncap-ctl init`; the socket's parent dir is created per
spec/paths.md § XDG layout; the log dir is created; a container with the
target name that exists but is not running is removed before launch.

The container invocation is spec/runtime.md § Launch command. Detached;
`exec` makes the Server the container's init process.

Readiness: after launch, poll the liveness predicate
(spec/runtime.md § Liveness) until live (deadline: `NCAP_TIMEOUT`, which
bounds both this readiness poll and the Server's drain grace — polls roughly
every 100ms). Losing a concurrent-start race ("name in use"): re-inspect —
running ⇒ success; dead ⇒ `rm` the container and start once more. Never
reaching readiness ⇒ fail loudly with the `inspect` state.

## Freshness and the digest

Freshness states, the digest algorithm, and the cache files are
spec/paths.md § Freshness and the digest and spec/paths.md § Cache contents.

Ensuring the Cache: fresh ⇒ nothing happens — no Nix evaluation. Stale or
missing ⇒ `nix print-dev-env --profile <cache>/profile <devshell>` runs on
the host (stderr inherited, stdout captured as the dump), its stdout written
to `<cache>/env`, the profile's history wiped after each eval (a failed prune
is non-fatal), and the new digest computed and stored.

The trigger surface — shell entry, direnv reload, the no-auto-refresh
consequences — is spec/flake-api.md § Freshness triggers.

## NCAP_* contract

| Var | Derive | Description |
| --- | --- | --- |
| `NCAP_PROJECT` | via `setup-env` from `NCAP_PROJECT_ROOT` (spec/paths.md § Project name) | Project name. Set by `project`. |
| `NCAP_PROJECT_ROOT` | `-` | Project root — the git toplevel of the consumer's checkout, falling back to the current directory outside a git repo; anchors the project name, the workspace mount, and the workdir of executed commands. No option: shellHook sets it. |
| `NCAP_SOCKET` | via `setup-env` from the project name (spec/paths.md § XDG layout) | Socket path. Set by `socketPath`. |
| `NCAP_CACHE_DIR` | via `setup-env` from the project name (spec/paths.md § XDG layout) | Cache dir. Set by `cacheDir`. |
| `NCAP_LOG_DIR` | via `setup-env` from the project name (spec/paths.md § XDG layout) | Log dir. Set by `logDir`. |
| `NCAP_CONTAINER` | via `setup-env` as `ncap-<project>` (spec/paths.md § Project name) | Container name. Set by `containerName`. |
| `NCAP_IMAGE` | `-` | OCI image; provides only the kernel/userland sandbox. Set by `image`. |
| `NCAP_RUNTIME` | `-` | OCI runtime (spec/runtime.md § Runtime adapter). Set by `runtime`. |
| `NCAP_DEVSHELL` | `-` | Complete flake URI of the Container shell; no bare-name `.#` prefixing is performed. Set by `devShell`. |
| `NCAP_RUN_OPTS` | `-` | JSON array of extra runtime args, appended after the default mounts (spec/runtime.md § Mounts). Set by `extraOptions`. |
| `NCAP_WATCH_FILES` | `-` | JSON array of project-root-relative watched files hashed for freshness (spec/paths.md § Freshness and the digest); also emitted as direnv `watch_file` calls (spec/flake-api.md § Freshness triggers). Set by `watchFiles`. |
| `NCAP_ENV_FORWARD` | `-` | JSON array of forwarded variable names (validated by the Client only, not by `ctl`; additionally consumed by the Client per spec/env.md § Merge rules). Set by `envForward`. |
| `NCAP_SERVER` | `-` | Store path of `ncap-server`. No option: pkgs-provided. |
| `NCAP_NIX` | `-` | Store path of `nix`. No option: pkgs-provided. |
| `NCAP_BASH` | `-` | Store path of devshell bash. No option: pkgs-provided. |
| `NCAP_TIMEOUT` | `-` | Seconds (`0` allowed: no drain grace / immediate readiness deadline); bounds start readiness and the Server's drain grace. Set by `timeout`. |
| `NCAP_HARDEN` | `-` | `true`/`false` enables harden (spec/runtime.md § Harden). Set by `harden`. |
| `NCAP_LOG_LEVEL` | `-` | Minimum severity the Server logs at; one of `debug`, `info`, `warning`, `error` (spec/server.md § Logging). Set by `logLevel`. |

Empty-string values count as unset. Every command except `setup-env`
demands the full `ctl` set: `NCAP_PROJECT_ROOT`, `NCAP_PROJECT`,
`NCAP_CONTAINER`, `NCAP_SOCKET`, `NCAP_CACHE_DIR`, `NCAP_LOG_DIR`,
`NCAP_IMAGE`, `NCAP_RUNTIME`, `NCAP_DEVSHELL`, `NCAP_NIX`, `NCAP_SERVER`,
`NCAP_BASH`, `NCAP_TIMEOUT`, `NCAP_WATCH_FILES`, `NCAP_RUN_OPTS`,
`NCAP_HARDEN`, and `NCAP_LOG_LEVEL`. A missing var is an error naming
the var, and a set
`NCAP_WATCH_FILES`/`NCAP_RUN_OPTS` that is not a JSON array of strings
is an error naming the var; a `NCAP_HARDEN` that is not `true`/`false`
is likewise an error; a `NCAP_LOG_LEVEL` that is not one of the four
levels is likewise an error. `NCAP_TIMEOUT` must parse as a non-negative
integer number of seconds. Each `NCAP_WATCH_FILES` entry must be
project-root-relative — an absolute entry or one containing a `..`
component is an error naming the var and the entry — and an entry that
exists must be a file (absent entries hash their absence; a broken
symlink counts as absent), avoiding a directory entry that would
permanently read stale. `NCAP_ENV_FORWARD` is validated by the Client
only. `setup-env` needs only `NCAP_PROJECT_ROOT`: it resolves
explicit-wins-else-derived per spec/paths.md § Project name and
spec/paths.md § XDG layout and prints
`export VAR='…'` lines (single-quote-escaped, fixed order `PROJECT`,
`CONTAINER`, `SOCKET`, `CACHE_DIR`, `LOG_DIR`) to stdout for the Host
shell to source — see spec/flake-api.md § shellHook. Failure exits
non-zero naming the var.

These are env-contract demands, not per-command usage errors: in
practice the flake sets all of them, so a missing var is a broken
environment rather than a usage error.

Precedence: explicit env wins, else the `setup-env`-derived value per `Derive`,
else an error naming the var. Ctl never overrides a set value. A missing
var whose `Derive` is `-` is an error naming it.
