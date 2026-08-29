# 04: Signals, process groups, disconnect cleanup

**What to build:** Interrupting feels local and leaves nothing behind. Children spawn in their own process group; the server delivers `Signal` frames with `kill(-pgid)` so grandchildren die with their progenitor. The client relays SIGINT/SIGTERM verbatim, one frame per event, and keeps streaming until the terminal frame; a client vanishing before the terminal frame gets its child SIGTERMed by group and reaped; no zombies accumulate under the server.

This ticket is now an index — the work is split into four agent-sized parts:

- **04a: Process groups + Signal delivery (server)** — `04a-process-groups-signals-server.md` — blocked by 02
- **04b: Disconnect cleanup (server)** — `04b-disconnect-cleanup.md` — blocked by 04a
- **04c: Client signal relay (client)** — `04c-client-signal-relay.md` — blocked by 04a
- **04d: Orphan reaping (no zombies)** — `04d-orphan-reaping.md` — blocked by 04b

**Status:** split — this ticket is done when all four parts are.

Design decisions locked during the split (agents must not relitigate):

- Children get `process_group(0)` (setpgid), not `setsid` — std-native, no unsafe, and `kill(-pgid)` works identically; `spec/server.md` is updated to match.
- The server forwards any signal number verbatim — no whitelist; `kill` failures (ESRCH, EINVAL) are warnings, never `Error` frames.
- The client relays SIGINT/SIGTERM only; SIGQUIT/SIGHUP/SIGTSTP/SIGCONT keep default dispositions (client dies ⇒ 04b's cleanup takes over).
- Disconnect detection = failed send only; a silent child of a vanished client runs to completion (accepted limitation).
