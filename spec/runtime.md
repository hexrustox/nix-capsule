# Runtime and mounts

This file is the single source of truth for the OCI runtime adapter, the
container launch (mount set, ordering, launch command), liveness, and the
`harden` posture. spec/ctl.md § init flow and spec/ctl.md § start flow
reference it and never restate it.

## Runtime adapter

`NCAP_RUNTIME` names the OCI runtime: `podman`, `docker`, or `auto`
(any other value ⇒ error naming `NCAP_RUNTIME`). `auto` resolves at config
time, before any flow runs except `setup-env` (which needs only
`NCAP_PROJECT_ROOT` and never resolves the runtime): `podman` is picked when
an executable `podman` exists in a `PATH` directory — executable meaning a
regular file with any execute bit set, found via `PATH` splitting — else
`docker`, else an error naming `NCAP_RUNTIME`. The check is PATH existence
only — no probe that the binary runs, no caching across invocations. An
explicit `podman`/`docker` skips detection entirely. `auto` resolves to the
concrete name; all downstream behavior treats `auto` exactly as the resolved
value. Both runtimes speak the same argument surface; state probes use
Go-template `inspect` (`State.Running`, the JSON `State` for failure
reports). A spawn or parse failure of the running-probe counts as
not-running. Rootless operation is the assumption.

Linux only — same-path bind mounts, read-only `/nix`, and shared Unix
sockets rule out macOS (podman-machine's VM breaks path identity).

## Liveness

One predicate serves both the `init` liveness probe and the `start`
readiness poll — liveness and readiness are the same check at different call
sites. Live means both: `<runtime> inspect` reports `State.Running`, and the
socket is connectable. The container reports `Running` while still sourcing
the Env dump ahead of the Server's bind, so `Running` alone is not live.

## Mounts

Defaults first; `extraOptions` args are appended after, with `$VAR`/`${VAR}`
expanded at launch (so `$CARGO_HOME`-style mounts work). Expansion happens in
Ctl, against its own environment; no word-splitting afterwards — a value
containing spaces stays one argument. A referenced unset variable is a launch
error naming it (an empty value expands to nothing). A `$` that starts neither
`${NAME}` nor `$NAME` (name = `[A-Za-z_][A-Za-z0-9_]*`) stays literal: a lone
`$`, `$$`, `$5`, `$-x`, `${}`, `${5}`, `${foo-bar}`, an unterminated `${…`,
and any other non-name body never consult the environment and never error.

Build order (exact — harden flags prepend, watches slot after the fixed
mounts, extra options append):

1. when `harden`: `--cap-drop=all --security-opt=no-new-privileges`;
2. `/nix:/nix` ro;
3. socket dir → same path, rw;
4. `<project root>` → same path, rw;
5. `-w <project root>`;
6. cache dir → same path, ro — **read-only so the container can't poison
   files the host will later source**;
7. log dir → same path, rw;
8. `<project root>/.git` → same path, ro, only when it is a real directory —
   judged without following symlinks, so worktree gitfiles (plain files) and
   symlinks (even to dirs) are skipped;
9. when `harden`: every watched file that exists → same path, ro (a broken
   symlink counts as absent);
10. expanded `extraOptions`, one argument per entry, in order.

Host paths are valid inside the container only because the project root is
bind-mounted at the same absolute path; anything not mounted is invisible
inside.

## Launch command

The container invocation:

```
<runtime> run -d --name <container> <mounts and options> -- <image> <NCAP_BASH> \
  -c "source '<cache>/env' && exec '<NCAP_SERVER>' --socket '<socket>' --log-dir '<log-dir>' --timeout <timeout> --log-level <log-level>"
```

`<NCAP_BASH>` and `<NCAP_SERVER>` are the absolute store paths from
`NCAP_BASH` and `NCAP_SERVER` — the image provides only the sandbox, so the
launch must not rely on its `PATH` for these binaries. Every interpolated
path is single-quote-escaped (`'` → `'\''`); timeout and log level render
bare. Detached; `exec` makes the Server the container's init process.

A socket path with no parent directory is a launch error naming the socket.
A container with the target name that exists but is not running is removed
before launch. Losing a concurrent-start race (failure output mentioning
"name in use", case-insensitive): re-inspect — running ⇒ success; dead ⇒
`rm` the container and start once more.

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
