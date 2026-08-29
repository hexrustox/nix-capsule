# nix-capsule — server (`ncap-server`)

Runs inside the container as its init process — the launcher is `bash -c "source <cache>/env && exec ncap-server …"`, whose trailing `exec` makes the server the process the runtime tracks, so the server inherits the container shell's environment from the sourced env dump and children resolve its tools through plain `PATH`.

## Startup

CLI: `--socket`, `--log-dir`, `--timeout` (drain grace, seconds).

- If the socket path exists: probe it. Connectable ⇒ another server owns it — error out. Stale (connect fails) ⇒ remove the file and bind.
- Logs to `<log-dir>/ncap-server-<epoch>.log`.

## Connection handling

One connection = one child; connections are handled concurrently.

1. Expect `Request` as the first frame; anything else ⇒ `Error` and close.
2. Send `Version`.
3. Spawn the child:
   - `command` + `args` from the request,
   - `cwd` from the request — must be a valid path inside the container; the same-path mount contract makes host cwds work, anything else fails with `Error`,
   - request env applied over the inherited devshell env (`Command::envs`),
   - spawned in its own process group (`setsid`) — every signal below targets the group (`kill(-pgid, …)`), so grandchildren die with their progenitor,
   - all three stdio pipes. Never a TTY (accepted limitation).
4. Bridge:
   - `Stdin` frames → child stdin. Client write-half close ⇒ drop the pipe (child sees EOF).
   - child stdout → `Stdout`, stderr → `Stderr`, read in chunks; each stream is FIFO, cross-stream order is not guaranteed.
   - `Signal` frames → `kill(-pgid, sig)`. Out-of-range signal numbers and signals for an already-exited child are ignored with a warning log.
5. Child exits ⇒ send `Exit { code | signal }` and close. Both fields null happens only if the exit status is somehow unknowable.

Spawn failure with `ENOENT` ⇒ `Exit { code: 127 }`; with `EACCES` ⇒ `Exit { code: 126 }` — terminal frame only, no `Error` (the client synthesizes the message). Any other spawn failure, invalid `Request`, or decode failure ⇒ `Error { message }` and close.

## Disconnect and shutdown

- **Client disconnect before the terminal frame:** the child is not orphaned — SIGTERM its process group, reap it, keep serving other connections.
- A `waitpid(-1)` reaping loop runs continuously so no zombies accumulate; with a PID namespace, orphans reparent to the server as the namespace's init.
- **SIGTERM/SIGINT** (e.g. `ncap-ctl stop` signals the container's init process): send `ServerStopping` to every live connection, SIGTERM every child's process group, drain all connections within `--timeout` seconds, remove the socket file, exit. Connections that miss the deadline are dropped when the container tears down as the server — the container's init process — exits.
