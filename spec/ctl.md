# nix-capsule — lifecycle (`ncap-ctl`)

`ncap-ctl` runs on the host, inside the host devshell — configuration arrives as `NCAP_*` env vars (contract below). Refusal is per command: a command demands exactly the vars it uses and errors naming the missing one. `completions` demands nothing; `show-options` only `NCAP_RUN_OPTS`.

## Commands

| Command | Behavior |
| --- | --- |
| `init` | Entry point from the shellHook. Probe, hash-check, then start or re-eval + restart (flow below). |
| `start` | Probe `<runtime> inspect`: running ⇒ done ("already running"). Not running ⇒ run the container detached, then await `State.Running`. |
| `stop` | `<runtime> stop <name>` — SIGTERM to PID 1, graceful drain. Idempotent: not running ⇒ success. |
| `restart` | Non-fatal `stop`, then `init` (which re-ensures the cache before starting). |
| `enter` | `<runtime> exec -it <name> <nix-bash> -c "source <cache>/env && exec <nix-bash>"` — interactive escape hatch, outside the protocol. Container down ⇒ error suggesting `ncap-ctl init`. |
| `status` | Container running? Socket connectable? Cache fresh/stale/missing? |
| `log` | Open the newest server log in `$PAGER` (fallback `less -R`). |
| `clean` | Stop the container, delete this project's cache, state, and runtime dirs. |
| `completions <shell>` | Shell completions. |
| `show-options` | Print the `$VAR`-expanded contents of `NCAP_RUN_OPTS`, one arg per line. |

### `init` flow

1. Read the stamp guard (below).
2. Probe liveness: `<runtime> inspect` → `State.Running`. The server is PID 1, so a running container is a live server — no socket polling needed.
   - Running and hash fresh ⇒ done.
   - Running and hash stale ⇒ re-eval, restart.
   - Not running ⇒ ensure cache (eval if hash stale or missing), start.

### `start` flow

The container invocation:

```
<runtime> run -d <mounts and options> -- <image> <nix-bash> \
  -c "source <cache>/env && exec ncap-server --socket … --log-dir … --timeout …"
```

Detached; the server becomes PID 1 via `exec`.

Readiness: after launch, poll `inspect` until `State.Running` (deadline: `timeout`). Losing a concurrent-start race ("name in use"): re-inspect — running ⇒ success; dead ⇒ `rm` the container and start once more. Never reaching `Running` ⇒ fail loudly with the `inspect` state and the tail of the newest server log.

## Mounts

Defaults first; `extraOptions` args are appended after, with `$VAR`/`${VAR}` expanded at launch (so `$CARGO_HOME`-style mounts work). Expansion happens in `ncap-ctl`, against its own environment; no word-splitting afterwards; a referenced unset variable is a launch error naming it.

| Mount | Mode | Purpose |
| --- | --- | --- |
| `/nix:/nix` | ro | Host store: bash, `ncap-server`, every devshell tool. |
| socket dir → same path | rw | Shared Unix socket at the identical path. |
| `<project root>` → same path | rw | Workspace at the identical path. |
| `-w <project root>` | — | Container working directory. |
| cache dir → same path | ro | Env dump the server sources — **read-only so the container can't poison files the host will later source**. |
| log dir → same path | rw | Server writes its JSON logs there. |
| `<project root>/.git` → same path | ro, if it exists | Read-only git metadata for tools that read it. |
| `<project root>/<watchFiles entries>` → same path | ro, if present and `harden = true` | Keeps watched files immutable from inside the container (see `harden` below). |

`harden = true` prepends `--cap-drop=all --security-opt=no-new-privileges` and bind-mounts every `watchFiles` entry read-only (when present) over the read-write project-root mount — a more-specific mount wins in podman/docker, so the files stay immutable even though the parent directory is writable. This prevents a contained process from rewriting files whose change triggers host-side re-evaluation (`nix print-dev-env` on the host, `shellHook` included) and re-sourcing inside the container. `flake.lock` edits and `nix flake update` must then happen on the host; accepted tradeoff, like `.git` being read-only. Capability drops occasionally break dev tooling, so hardening stays opt-in.

## Runtime adapter

`NCAP_RUNTIME` names the OCI runtime: `podman` (default), `docker`, or an absolute path. Both runtimes speak the same argument surface; state probes use Go-template `inspect` (`State.Running`, `Id`). Rootless operation is the assumption.

## `NCAP_*` contract

Set by `mkShell`; consumed by `ncap-ctl` (and `NCAP_SOCKET`, `NCAP_ENV_FORWARD` by `ncap`):

| Var | Contents |
| --- | --- |
| `NCAP_PROJECT` | project name |
| `NCAP_SOCKET` | socket path |
| `NCAP_CACHE_DIR` | cache dir |
| `NCAP_LOG_DIR` | log dir |
| `NCAP_CONTAINER` | container name |
| `NCAP_IMAGE` | image |
| `NCAP_RUNTIME` | runtime |
| `NCAP_RUN_OPTS` | JSON array of extra runtime args |
| `NCAP_WATCH_FILES` | JSON array of hashed files |
| `NCAP_SERVER` | store path of `ncap-server` |
| `NCAP_NIX` | store path of `nix` |
| `NCAP_BASH` | store path of devshell bash |
| `NCAP_TIMEOUT` | drain grace, seconds |
| `NCAP_PROJECT_ROOT` | project root |

## Stamp guard

The first thing `init`/`start`/`restart` do with the cache: read `<cache>/project`. Present and different from the current project root ⇒ error — "project name `<name>` is already keyed to root `<path>`; set `project`". Absent ⇒ write it. `clean` deletes the whole project-keyed cache and state dirs, stamp included.
