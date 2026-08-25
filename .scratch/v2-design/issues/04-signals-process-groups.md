# 04: Signals, process groups, disconnect cleanup

**What to build:** Interrupting feels local and leaves nothing behind. Children spawn in their own process group; the server delivers `Signal` frames with `kill(-pgid)` so grandchildren die with their progenitor. The host terminal's Ctrl-C reaches the client's own handler — it never becomes stdin bytes — and travels as a `Signal` frame: first SIGINT/SIGTERM is graceful (the child may trap and clean up; the client keeps streaming until `Exit`), any second signal before `Exit` escalates to SIGKILL with ~2 s grace, after which the client gives up and exits 130 regardless. Out-of-range signal numbers, signals for an already-exited child, and signals arriving after escalation are ignored with a warning log.

Cleanup paths: a client disconnecting before the terminal frame gets its child SIGTERMed by group and reaped while the server keeps serving other connections; the server runs a continuous orphan-reaping loop so no zombies accumulate beneath PID 1.

**Blocked by:** 02 (First exec through the socket).

**Status:** ready-for-agent

- [ ] Ctrl-C (SIGINT to the client) interrupts the child promptly; a trapping child observes SIGINT and its cleanup runs; client exits per the child's outcome
- [ ] Second signal before `Exit` escalates to SIGKILL; if no `Exit` arrives within the grace period the client still exits 130
- [ ] Signal reaches the whole group: a child that spawned its own subprocess loses both
- [ ] Abrupt client disconnect terminates the child process group; the next connection still works
- [ ] Many sequential children leave no zombies (reaping loop holds)
- [ ] Out-of-range / late / post-exit signals are ignored with a warning; the connection continues to its normal `Exit`
