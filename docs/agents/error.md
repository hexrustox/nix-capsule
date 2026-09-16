# Error Messages

Every error message in this repo speaks one voice. These rules govern *how*
messages read — what errors exist and when they fire comes from `spec/`.

Scope: all user-visible messages — `thiserror` `#[error(...)]` attributes and
`eprintln!` diagnostics. Ad-hoc `String` errors are not in scope because they
are banned outright (see [No string errors](#no-string-errors)).

## Message shape

One sentence fragment, starting lowercase, ending without a period.

- good: `` missing `name` field ``
- bad: `The name field was not provided.`

## Tokens

Backtick what the message names: flags, env vars, field names, frame names,
format names, paths. Leave raw data unquoted: numbers, counts.

- good: `` unknown option `--recursive` ``
- good: `` frame declares a 1000009-byte payload above the 16 MiB cap ``
- bad: `` list index `3` out of range `` — `3` is data, not a token

## Context chaining

There is no context-carrying error crate in this repo; context chains through
`thiserror`'s `{source}` rendering as a colon-joined `context: cause`.
Rules for the chain:

- State what is wrong + the subject first; the underlying cause rides last
  after a colon: ``cannot connect to socket `{socket}`: {source}``
  (src/client.rs:52).
- Wire it with a `#[source]` field and a `{source}` placeholder; use
  `#[error(transparent)]` for pass-through variants (src/ctl/config.rs:85).
- The colon-join is for `context: cause` only. Don't append unrelated clauses
  or multiple colon-separated causes.
- When `{source}` is a captured command's stdout/stderr, chain it with a
  newline instead of a colon-space, since the output may be multi-line. If
  instead the command's stdout/stderr is printed straight through to the
  user's terminal, attach no source at all — the context line stands alone.

## Advice lives outside the message

The message states what is wrong and the facts needed to act — never the fix.
Prescriptive advice is carried outside the message text:

- on the **cause's own message**, which the chain renders after the colon, or
- as a **separate sibling sentence** printed by the caller when nothing
  upstream carries it, or
- dropped entirely when the cause already implies it.

- message: `` cannot open `conf.yaml` ``
- advice: `` create the file first, or point `--config` at an existing path ``
- bad: `` cannot open `conf.yaml`: create the file first ``

## Rendering

- Errors render once, on stderr, at the program's entry point and exit 1.
  Errors return up the stack; `eprintln!()` fires only there, never mid-task
  in command or library code — command handlers surface errors and notices by
  returning values the toplevel renders.
- Every rendered line carries the binary name as prefix
  (`ncap: {err}` / `ncap-ctl: {err}` / `ncap-server: {err}`); a prefix-less
  error is a bug.
- Progress notices (`evaluating devshell \`{devshell}\` with nix
  print-dev-env...`) are their own prefix-carrying stderr lines produced at
  the toplevel — never mixed into error output.
- If an error may be an internal bug rather than a user mistake, say so in a
  separate sentence when surfaced, never inside the message text
  (`this is likely an internal error`).

## No string errors

Errors carry structure: `thiserror` enums with per-variant `#[error(...)]`
messages. Ad-hoc `Result<_, String>` return types and error prose built with
`format!` or `err.to_string()` are forbidden — they defeat the `{source}`
chain, make messages ungreppable, and drift from the one-voice rules.

```rust
// good
#[error("cannot read `{path}`: {source}")]
Read { path: PathBuf, #[source] source: io::Error }

// bad — string error
.map_err(|err| format!("cannot eval `{devshell}`: {err}"))?;
```

All io errors get context: a variant names the failed operation and its
object, with the raw `io::Error` riding as a `#[source]` field. Leaf
pass-through io variants with `#[error("{0}")]` are banned.

## Good vs bad

| good | bad |
| --- | --- |
| `` cannot connect to socket `{socket}` `` + `: {source}` | `Failed to establish a connection to the socket.` |
| `` missing `name` field `` | `The name field was not provided.` |
| `` unknown option `--recursive` `` | `error: you must use a valid option` |
| `` frame declares a 1000009-byte payload above the 16 MiB cap `` | `` frame declares a `1000009`-byte payload `` |
| `` cannot derive a project name from root `/tmp/proj` `` | `` cannot derive a project name from root `/tmp/proj`; set `project` `` |
| `` cannot read `{path}`: {source} `` | `` #[error("{0}")] Io(io::Error) `` — bare io pass-through, no context |
| `.map_err(...)` into a `thiserror` variant | `Result<(), String>` or `format!("... failed: {err}")` |
| `` ncap-ctl: container `proj` is not running `` (toplevel, prefixed) | `` container `proj` is not running `` mid-task, prefix-less |
| `` cannot eval `{devshell}` with print-dev-env: `` + newline + output | `` cannot eval `x`: multi-line output crammed onto one colon-line `` |
