# Server (`ncap-server`)

See [`main.md`](main.md) for the wire protocol reference.

## CLI

```
ncap-server [OPTIONS]
```

| Option | Description |
|--------|-------------|
| `--socket <PATH>` / `-s` | Unix socket path to listen on. |
| `--log-dir <PATH>` / `-l` | Log directory (default: socket parent directory). |
| `--timeout <SECONDS>` | Drain timeout for active connections on shutdown. |

## Logging

Structured JSON logs via `tracing` + `tracing-appender`. Each run creates `ncap-server-<epoch>.log` in the log directory. Log level defaults to `info`, overridable via `RUST_LOG`.

## Socket Lifecycle

1. Remove any stale socket file at the configured path.
2. Listen. Accept connections in a loop, each handled in a separate `tokio::spawn` task.
3. On SIGTERM/SIGINT: broadcast `ServerStopping` to all active connections, stop accepting, wait up to `--timeout` seconds for connections to drain, then exit.

## Connection Handling

1. Read the first frame. Must be `Request` — otherwise send `Error` and close.
2. Parse `Request`. Send a `Version` frame with the server's package version.
3. If `request.version` differs from the server's version (or is absent), log a warning.
4. Spawn the child process with the given command, args, cwd, and env overrides (applied on top of the inherited devshell environment).
5. If spawn fails, send `Error` and close.
6. Bridge I/O:
   - Client `Stdin` frames → child's stdin.
   - Child's stdout → `Stdout` frames.
   - Child's stderr → `Stderr` frames.
7. On child exit, send `Exit` with the exit code (`i32`).
8. On wait error, send `Error`.

## Child Process Semantics

- Inherits the devshell environment (server runs inside a sourced `nix print-dev-env`).
- Env overrides from the `Request` are applied on top.
- Server never `exec`s the child.
- Child runs with piped stdio (not a TTY).
