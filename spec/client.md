# Client (`ncap`)

See [`main.md`](main.md) for the wire protocol reference.

## CLI

```
ncap [OPTIONS] <COMMAND> [ARGS...]
```

| Option | Description |
|--------|-------------|
| `--socket <PATH>` / `-s` | Unix socket path. Reads from `NCAP_SOCKET`. |
| `--env <KEY[=VALUE]>` / `-e` | Environment overrides. `KEY=VALUE` sent as-is; bare `KEY` looked up from host env. May repeat. |
| `--cwd <PATH>` / `-w` | Override working directory. Defaults to client's current directory. |
| `<COMMAND> [ARGS...]` | Command and arguments to execute (required, trailing). |

## Behavior

1. Parse CLI, resolve `--env` overrides, determine `cwd`.
2. Connect to the Unix socket.
3. Send `Request` frame with `version` set to the package version.
4. Spawn a thread to read stdin into a bounded channel (capacity 32, 8 KiB buffer), forwarded as `Stdin` frames.
5. Read frames from the server:
   - **Version** — On receipt, mark version as received. If the server's version differs from the client's, print a warning to stderr.
   - **Stdout** — Write to stdout.
   - **Stderr** — Write to stderr.
   - **Exit** — Record exit code, break loop.
   - **Error** — Report error, exit with code 1.
   - **ServerStopping** — Print reason, exit with code 143.
6. If no `Version` frame was received before the connection ends, print a warning to stderr.

## Exit Codes

| Condition | Code |
|-----------|------|
| Child exit | Child's exit code (truncated to u8) |
| Server error / connection failure | 1 |
| Server shutting down / connection drop without Exit | 143 (128 + SIGTERM) |
