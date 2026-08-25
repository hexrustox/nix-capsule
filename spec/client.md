# nix-capsule — client (`ncap`)

## CLI

```
ncap [--socket PATH | $NCAP_SOCKET] [--env KEY[=VALUE]]… [--cwd PATH] [--] COMMAND [ARGS…]
ncap completions <shell>
```

- `--env KEY=VALUE` sets an override (env layer 4). Bare `KEY` copies that variable from the client's environment if set; silently omitted otherwise.
- `cwd` defaults to the client's current directory. The same-path mount contract makes it valid inside the container.
- Connect failure errors with a static hint: the socket path and a suggestion to run `ncap-ctl init`. No implicit auto-start — init may need to evaluate Nix, and surprises are worse than a hint.
- Before sending `Request`, the client resolves every name in `NCAP_ENV_FORWARD` from its own environment into the request env (layer 2). Values are read per invocation — a changed host value reaches the next command without any restart.

## stdin

A blocking reader thread pumps host stdin into `Stdin` frames. The client never allocates a TTY and never puts the terminal in raw mode — Ctrl-C reaches the client's own signal handler, not the child.

## Signals

The child lives in another PID namespace: host terminal signals never reach it directly. The protocol is the only path.

- **First SIGINT or SIGTERM** (Ctrl-C in the terminal): send `Signal { sig }`, keep streaming until `Exit` — a graceful interrupt the child may trap and clean up after.
- **Any second signal before `Exit`:** send `Signal { 9 }` (SIGKILL), wait ~2 s for `Exit`, then give up and exit 130 regardless.
- Killing the client outright (or closing the terminal) drops the connection; the server SIGTERMs the child.

## Exit codes

| Condition | Client exit code |
| --- | --- |
| Child exited | the child's code, truncated to u8 (the shell's own ceiling) |
| Child killed by signal | `128 + signal` |
| Terminal frame is `Error`, or transport/decode failure | `1` |
| `ServerStopping`, or socket closed without a terminal frame | `143` (128+SIGTERM) |
| Escalation: SIGKILL sent, no `Exit` within the grace period | `130` (128+SIGINT) |

## Wrappers

`mkShell` writes one `writeShellScriptBin` per wrapper entry, shadowing real binaries on PATH: `exec ncap <command> "$@"` plus the entry's `env`/`cwd` flags. Wrapped and bare invocations share every behavior above — a wrapper is just a pre-filled command line.
