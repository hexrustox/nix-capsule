# nix-capsule — client (`ncap`)

## CLI

```
ncap [--socket PATH | $NCAP_SOCKET] [--env KEY[=VALUE]]… [--cwd PATH] [--] COMMAND [ARGS…]
ncap completions <shell>
```

- `--env KEY=VALUE` sets an override (env layer 4). Bare `KEY` copies that variable from the client's environment if set; silently omitted otherwise.
- `cwd` defaults to the client's current directory. The same-path mount contract makes it valid inside the container.
- Connect failure errors with a static hint: the socket path and a suggestion to run `ncap-ctl init`. No implicit auto-start — init may need to evaluate Nix, and surprises are worse than a hint.
- Before sending `Request`, the client resolves every name in `NCAP_ENV_FORWARD` (a JSON array of variable names, like the other `NCAP_*` arrays) from its own environment into the request env (layer 2). Values are read per invocation — a changed host value reaches the next command without any restart.

## stdin

A blocking reader thread pumps host stdin into `Stdin` frames. The client never allocates a TTY and never puts the terminal in raw mode — Ctrl-C reaches the client's own signal handler, not the child.

## Signals

The child is not attached to the host terminal: terminal signals never reach it directly. The protocol is the only path.

- **Signal relay:** on SIGINT, SIGTERM, SIGQUIT, or SIGHUP the client sends `Signal { sig }` verbatim and keeps streaming until `Exit`. The child may trap and clean up after, or ignore it; that grace is the child's, not the client's.
- Job-control signals (SIGTSTP, SIGCONT) are not relayed: with no TTY on either side, stopping the child while the client keeps streaming would leave a half-suspended session. Accepted limitation.
- Killing the client outright (or closing the terminal) drops the connection; the server SIGTERMs the child's process group.

## Exit codes

| Condition | Client exit code |
| --- | --- |
| Child exited | the child's code, truncated to u8 (the shell's own ceiling) |
| Child killed by signal | `128 + signal` |
| Spawn failed with `ENOENT` / `EACCES` (`Exit { code: 127 \| 126 }`, no `Error` frame) | prints `ncap: <command>: command not found` / `ncap: <command>: permission denied` to stderr; exits `127` / `126` |
| `Exit { null, null }` (status unknowable) | warning on stderr, then `1` |
| Terminal frame is `Error`, or transport/decode failure | `1` |
| `ServerStopping` received — the client bails immediately and stops streaming — or socket closed without a terminal frame | `143` (128+SIGTERM) |

## Wrappers

`mkShell` writes one `writeShellScriptBin` per wrapper entry, shadowing real binaries on PATH: `exec ncap <command> "$@"` plus the entry's `env`/`cwd` flags. Wrapped and bare invocations share every behavior above — a wrapper is just a pre-filled command line.
