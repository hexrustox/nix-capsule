# Log Format

`spec/server.md` § Logging is the source of truth: it specifies the log
file's mechanism and inventories which events the Server logs. This doc is
complementary — it governs only *how* lines read and is the place to update
when format guidance changes.

## Line shape

Every line is one event:

```
[YYYY-MM-DDTHH:MM:SSZ] level: message
```

- the timestamp form (`[YYYY-MM-DDTHH:MM:SSZ]`) is specified in
  `spec/server.md` § Logging
- `level` is a lowercase full word — never an abbreviation (`warning`, not
  `warn`) — so lines stay greppable the way error messages are
- one line per event, appended; a message that carries captured output
  (command stdout/stderr) doesn't exist in this log — captured output rides
  in `Error` frames, not log lines

## Levels

| level | category |
| --- | --- |
| `debug` | per-operation chatter |
| `info` | lifecycle transitions |
| `warning` | degraded but survivable |
| `error` | failed operation |

`spec/server.md` § Logging maps each concrete event to its level by
category. Add events via spec, then pick the level by category before
writing the line.

## Message voice

The same one voice as error messages: a sentence fragment, starting
lowercase, ending without a period. Backtick tokens (flags, field names,
frame names, paths); leave raw data unquoted. The message states what
happened and the facts needed to know it happened — never advice.

- good: `` connection from agent version 0.8.0 mismatched the Server's 0.9.0 ``
- bad: `The connected agent runs a different version of nix-capsule.`
- bad: `` WARNING — connected agent has an unexpected version ``

## Writing

Mechanics — the log file's path/naming, the RFC 3339 stamp, stderr
mirroring, and best-effort writes — are specified in `spec/server.md`
§ Logging. One voice rule the spec leaves to this doc: log lines carry no
binary prefix — the file identifies the writer — unlike rendered errors
(docs/agents/error.md § Rendering).

## Good vs bad

| good | bad |
| --- | --- |
| `` [2026-09-15T12:00:00Z] info: socket bound `` | `[2026-09-15T12:00:00Z] Server started.` |
| `` ... warning: agent reported version 0.8.0 against the Server's 0.9.0 `` | `... WARN: version conflict detected` |
| `` ... debug: request declares cwd `~/proj` serving 1 mount `` | `... debug: got request with Lots Of Details relevance unclear` |
| one event, one line | `error: cannot bind: ...` followed by a prose paragraph on the same event |
| `Log::line` best-effort writes | panicking or failing the Connection because the log write failed |
