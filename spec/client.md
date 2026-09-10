# Client (`ncap`)

The Client is the Host shell's entry to the Container: it sends one command
per Connection to the Server and streams the Child's stdio back to the
terminal. The wire behavior it relies on is spec/protocol.md § Frames; the Host shell
that provides `ncap` and its Wrappers is spec/flake-api.md § mkShell.

## CLI

```
ncap [--socket PATH | $NCAP_SOCKET] [--env KEY[=VALUE]]… [--cwd PATH] [--] COMMAND [ARGS…]
ncap completions <shell>
```

- `--socket` is required; `NCAP_SOCKET` supplies the default. Neither present
  is a usage error.
- `--env KEY=VALUE` sets an override — layer 4 of the environment layering
  (spec/flake-api.md § Environment layering). Bare `KEY` copies that variable
  from the Client's environment if set; silently omitted otherwise. `KEY=` is an explicit empty
  value; a value may itself contain `=`; an empty key is silently omitted.
- `--cwd` defaults to the Client's current directory. The same-path mount
  contract (spec/ctl.md § Mounts) makes host cwds valid inside the container;
  anything not mounted is invisible — the Server reports the failure.
- Connect failure errors with a static hint naming the socket path and
  suggesting `ncap-ctl init`. No implicit auto-start — init may need to
  evaluate Nix, and surprises are worse than a hint.
- `completions <shell>` prints a completion script for the named shell
  (`bash`, `zsh`, and `fish` are required; other shells optional) and demands
  nothing else.

## Request construction

- The request env is the merge of, in order: every name in `NCAP_ENV_FORWARD`
  (a JSON array of variable names) resolved from this process, then every
  `--env` flag — later wins by key, deduplicated, unset entries silently
  omitted. The authoritative merge rules are spec/flake-api.md § Environment layering.
  Values are read per invocation — a changed host value reaches the
  next command without any restart.
- `NCAP_ENV_FORWARD` that is not a JSON array of names is a local error
  (exit `1`).
- The request carries the Client's version.
- `cwd` defaults to the Client's current directory (see CLI).

## stdin

A blocking reader thread pumps host stdin into `Stdin` frames. EOF on host
stdin is one empty `Stdin` frame — the socket's write half stays open so the
signal relay keeps working; the frame is best-effort, since a Child that
finished first never needs it (a failed send is not fatal). The Client never
allocates a TTY and never puts the terminal in raw mode — Ctrl-C reaches the
Client's own signal handler, not the Child.

## Signals

The Child is not attached to the host terminal: terminal signals never reach
it directly. The protocol is the only path.

- **Signal relay:** on SIGINT or SIGTERM the Client sends
  `Signal { signal }` verbatim — one frame per event, repeated signals
  forwarded repeatedly — and keeps streaming until the terminal frame, then
  exits per the Child's outcome. The Child may trap and clean up after, or
  ignore it; that grace is the Child's, not the Client's. The Client never
  interprets a signal.
- Every other signal keeps its default disposition (accepted limitation):
  SIGQUIT, SIGHUP, and closing the terminal kill the Client outright,
  dropping the Connection so the Server TERMs the Child's process group;
  job-control signals (SIGTSTP, SIGCONT) are not relayed — with no TTY on
  either side, stopping the Child while the Client keeps streaming would
  leave a half-suspended session.

## Version warnings

- A `Version` frame naming a different version than the Client's own is a
  warning on stderr, naming both versions.
- Reaching the terminal frame without ever receiving a `Version` frame is a
  warning on stderr.
- Neither is ever a rejection.

## Exit codes

| Condition | Client exit code |
| --- | --- |
| Child exited normally | the Child's code (it travels as u8) |
| Child killed by signal | `128 + signal` |
| Terminal `Exit` carries code `127` (spawn `ENOENT`, or a Child's own 127) | prints `ncap: <command>: command not found` to stderr; exits `127` |
| Terminal `Exit` carries code `126` (spawn `EACCES`, or a Child's own 126) | prints `ncap: <command>: permission denied` to stderr; exits `126` |
| Terminal `Exit` with neither field set (status unknowable) | warning on stderr, then `1` |
| Terminal frame is `Error`, a transport/decode failure, or a local failure (e.g. malformed `NCAP_ENV_FORWARD`) | `1` |
| `ServerStopping` received — the Client bails immediately and stops streaming — or the socket closed without a terminal frame | `143` (128 + SIGTERM) |
