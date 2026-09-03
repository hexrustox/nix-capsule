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
   - spawned in its own process group (`process_group(0)`, the child as its own group leader) — every signal below targets the group (`kill(-pgid, …)`), so grandchildren die with their progenitor,
   - all three stdio pipes. Never a TTY (accepted limitation).
4. Bridge:
   - `Stdin` frames → child stdin; an empty frame is stdin EOF — drop the pipe (child sees EOF) and keep the connection open for `Signal` frames. A client write-half close means the same.
   - child stdout → `Stdout`, stderr → `Stderr`, read in chunks; each stream is FIFO, cross-stream order is not guaranteed.
   - `Signal` frames → the signal number is forwarded verbatim to `kill(-pgid, sig)`; kill failures (an already-exited group, an out-of-range number) produce one warning on the server's stderr and the connection continues to its normal terminal frame.
5. Child exits ⇒ send `Exit { code | signal }` and close. Both fields null happens only if the exit status is somehow unknowable.

Spawn failure with `ENOENT` ⇒ `Exit { code: 127 }`; with `EACCES` ⇒ `Exit { code: 126 }` — terminal frame only, no `Error` (the client synthesizes the message). Any other spawn failure, invalid `Request`, or decode failure ⇒ `Error { message }` and close.

## Disconnect and shutdown

- **Client disconnect before the terminal frame:** the child is not orphaned — the only notice is a failed send, on which the server SIGTERMs the child's process group, reaps it, and keeps serving other connections. A silent child of a vanished client runs to completion (accepted limitation).
- Children are reaped without an explicit reaper: the connection task awaits its child, and the async runtime's process driver collects anything left behind, so no zombies accumulate. With a PID namespace, orphans reparent to the server as the namespace's init.
- **SIGTERM/SIGINT** (e.g. `ncap-ctl stop` signals the container's init process): send `ServerStopping` to every live connection, SIGTERM every child's process group, drain all connections within `--timeout` seconds, remove the socket file, exit. Connections that miss the deadline are dropped when the container tears down as the server — the container's init process — exits.
