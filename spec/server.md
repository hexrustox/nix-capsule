# nix-capsule — server (`ncap-server`)

Runs inside the container as PID 1 — the launcher is `bash -c "source <cache>/env && exec ncap-server …"`, so the server inherits the devshell environment from the sourced dump and children resolve devshell tools through plain `PATH`.

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
   - all three stdio pipes. Never a TTY (accepted limitation).
4. Bridge:
   - `Stdin` frames → child stdin. Client write-half close ⇒ drop the pipe (child sees EOF).
   - child stdout → `Stdout`, stderr → `Stderr`, read in chunks; each stream is FIFO, cross-stream order is not guaranteed.
   - `Signal` frames → `kill(child, sig)`. Out-of-range signal numbers are ignored with a warning log.
5. Child exits ⇒ send `Exit { code | signal }` and close. Both fields null happens only if the exit status is somehow unknowable.

Spawn failure, invalid `Request`, or decode failure ⇒ `Error { message }` and close.

## Disconnect and shutdown

- **Client disconnect before the terminal frame:** the child is not orphaned — SIGTERM it, reap, keep serving other connections.
- **SIGTERM/SIGINT** (e.g. `ncap-ctl stop` sends SIGTERM to PID 1): send `ServerStopping` to every live connection, SIGTERM every child, drain all connections within `--timeout` seconds, remove the socket file, exit. Connections that miss the deadline are dropped when the container tears down as PID 1 exits.
