# nix-capsule — lifecycle (`ncap-ctl`)

`ncap-ctl` runs on the host, inside the host devshell — all configuration arrives as `NCAP_*` env vars (contract below); it refuses to run without them.

## Commands

| Command | Behavior |
| --- | --- |
| `init` | Entry point from the shellHook. Probe, hash-check, then start or re-eval + restart (flow below). |
| `start` | Probe: connectable ⇒ no-op ("already running"). Stale socket file ⇒ remove it. Then run the container detached. |
| `stop` | `<runtime> stop <name>` — SIGTERM to PID 1, graceful drain. |
| `restart` | Non-fatal `stop`, then `init` (which re-ensures the cache before starting). |
| `enter` | `<runtime> exec -it <name> <nix-bash> -c "source <cache>/env && exec <nix-bash>"` — interactive escape hatch, outside the protocol. |
| `status` | Container running? Socket connectable? Cache fresh/stale/missing? |
| `log` | Open the newest server log in `$PAGER` (fallback `less -R`). |
| `clean` | Stop the container, delete this project's cache and state dirs. |
| `completions <shell>` | Shell completions. |
| `show-options` | Print the fully expanded runtime invocation, one arg per line. |

### `init` flow

1. Read the stamp guard (below).
2. Probe the socket.
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

## Mounts

Defaults first; `extraOptions` args are appended after, with `$VAR`/`${VAR}` expanded at launch (so `$CARGO_HOME`-style mounts work).

| Mount | Mode | Purpose |
| --- | --- | --- |
| `/nix:/nix` | ro | Host store: bash, `ncap-server`, every devshell tool. |
| socket dir → same path | rw | Shared Unix socket at the identical path. |
| `$PROJECT_ROOT` → same path | rw | Workspace at the identical path. |
| `-w $PROJECT_ROOT` | — | Container working directory. |
| cache dir → same path | ro | Env dump the server sources — **read-only so the container can't poison files the host will later source**. |
| log dir → same path | rw | Server writes its JSON logs there. |
| `$PROJECT_ROOT/.git` → same path | ro, if it exists | Read-only git metadata for tools that read it. |

`harden = true` prepends `--cap-drop=all --security-opt=no-new-privileges` (opt-in: capability drops occasionally break dev tooling).

## Runtime adapter

`NCAP_RUNTIME` names the OCI runtime: `podman` (default), `docker`, or an absolute path. Both runtimes speak the same argument surface; state probes use Go-template `inspect` (`State.Running`, `Id`). Rootless operation is the assumption.

## `NCAP_*` contract

Set by `mkShell`; consumed by `ncap-ctl` (and `NCAP_SOCKET` by `ncap`):

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
| `PROJECT_ROOT` | workspace root |

## Stamp guard

The first thing `init`/`start`/`restart` do with the cache: read `<cache>/project`. Present and different from `$PROJECT_ROOT` ⇒ error — "project name `<name>` is held by another checkout; set `project`". Absent ⇒ write it. `clean` deletes the whole project-keyed cache and state dirs, stamp included.
