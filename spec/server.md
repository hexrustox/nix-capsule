# Server (`ncap-server`)

The Server runs inside the container as its init process — the launcher is
`bash -c "source <cache>/env && exec ncap-server …"`, whose trailing `exec`
makes the Server the process the runtime tracks, so the Server inherits the
container shell's environment from the sourced Env dump and children resolve
its tools through plain `PATH`. No Nix evaluation, daemon, or image content
is needed inside the container — the image only provides a kernel and
userland sandbox. The container environment is a snapshot that changes only
on re-init (spec/ctl.md § init flow).

## Startup

CLI: `--socket`, `--log-dir`, `--timeout` (drain grace, seconds).

- If the socket path exists: probe it. Connectable ⇒ another Server owns it —
  error out naming the path, leaving the owner untouched. Stale (connect
  fails) ⇒ remove the file and bind.
- Logs to `<log-dir>/ncap-server-<epoch-millis>.log` (millisecond epochs keep
  runs started in the same second apart). Every line is timestamped and
  mirrored to stderr so the container runtime captures the same stream.
  Logging is best-effort and never disturbs the Connection it reports on.

## Connection handling

One Connection = one Child; connections are handled concurrently.

1. Expect `Request` as the first frame; anything else, or an undecodable
   frame, ⇒ `Error` and close. A client that leaves before its first frame is
   dropped silently.
2. Send `Version`.
3. Version comparison: a `Request` version differing from the Server's own is
   one warning line in the Server's log — advisory, never a rejection
   (spec/protocol.md § Guarantees).
4. Validate `cwd`: it must be an existing directory inside the container; the
   same-path mount contract (spec/ctl.md § Mounts) makes host cwds work,
   anything else fails with `Error`.
5. Spawn the Child:
   - `command` + `args` from the request, `cwd` from the request,
   - request env applied over the inherited Env dump — entries override
     same-named inherited keys and never clear inherited keys; an entry
     without `=` is an `Error` (invalid request env),
   - spawned as its own process group leader — every signal below targets
     the group (`kill(-pgid, …)`), so grandchildren die with their
     progenitor,
   - all three stdio pipes. Never a TTY (accepted limitation).
6. Spawn failure with `ENOENT` ⇒ `Exit { code: 127 }`; with `EACCES` ⇒
   `Exit { code: 126 }` — terminal frame only, no `Error` (the Client
   synthesizes the message). Any other spawn failure ⇒ `Error { message }`
   and close.
7. Bridge:
   - `Stdin` frames → Child stdin; an empty frame is stdin EOF — drop the
     pipe (Child sees EOF) and keep the Connection open for `Signal` frames.
     A client write-half close means the same. A failed write to the Child's
     stdin (pipe already closed) drops the stdin pipe; the Connection
     continues.
   - Child stdout → `Stdout`, stderr → `Stderr`, read in chunks; each stream
     is FIFO, cross-stream order is not guaranteed.
   - `Signal` frames → the signal number is forwarded verbatim to
     `kill(-pgid, sig)`; kill failures (an already-exited group, an
     out-of-range number) produce one warning in the Server's log and the
     Connection continues to its normal terminal frame.
8. Child exits ⇒ wait for it, send `Exit { code | signal }` and close. Both
   fields null happens only if the exit status is somehow unknowable.

## Disconnect before the terminal frame

- The Child is not orphaned — the only notice is a failed send, on which the
  Server TERMs the Child's process group and then awaits it, however long it
  takes: the grace after the TERM is the Child's, not the Server's, so there
  is no KILL escalation. Other connections keep being served.
- Accepted limitation: a silent Child — no output, stdin already EOF'd — of a
  vanished Client runs to completion; nothing observable triggers detection.
- No Child accumulates as a zombie: every spawned Child is awaited, including
  after a disconnect TERM. With a PID namespace, orphans reparent to the
  Server as the namespace's init.

## Shutdown

- **SIGTERM/SIGINT** (e.g. `ncap-ctl stop` signals the container's init
  process): send `ServerStopping` to every live Connection — including one
  still waiting for its first frame — then TERM every Child's process group.
  Each bridge keeps running, so a Child that finishes inside the drain grace
  still delivers its terminal frame.
- Drain all connections within `--timeout` seconds. Connections that miss the
  deadline are dropped when the container tears down, as the Server — the
  container's init process — exits.
- Remove the socket file and exit. The socket's parent directory stays.
