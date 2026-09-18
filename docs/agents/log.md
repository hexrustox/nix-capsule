# Log Format

`spec/server.md` § Logging is the source of truth: it specifies the log
file's mechanism and inventories which events the Server logs. This doc is
complementary — it governs only *how* lines read and is the place to update
when format guidance changes.

> **Every example below is from a fictitious project** — `wafflectl`, a
> manager of waffle irons. None of the messages, identifiers, or snippets is
> drawn from this codebase; they exist only to illustrate the rules.

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

- good: `` iron `iron-7` reported temperature 160°C against the expected 180°C ``
- bad: `The connected iron runs a different temperature profile.`
- bad: `` WARNING — iron temperature unexpected ``
- no binary prefix — the log file identifies the writer.

## Good vs bad

| good | bad |
| --- | --- |
| `` [2026-09-15T12:00:00Z] info: iron `iron-7` heated `` | `[2026-09-15T12:00:00Z] Iron heating complete.` |
| `` ... warning: iron reported batch 4 against the expected 5 `` | `... WARN: batch mismatch detected` |
| `` ... debug: heating hop 2 of 7 took 12ms `` | `... debug: got hop with Lots Of Details relevance unclear` |
| one event, one line | `error: cannot heat: ...` followed by a prose paragraph on the same event |
| best-effort writes; a failed write to the log is ignored | the logged operation fails because writing the log line failed |
