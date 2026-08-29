# 04: Signals, process groups, disconnect cleanup

**What to build:** Interrupting feels local and leaves nothing behind. Children spawn in their own process group; the server delivers `Signal` frames with `kill(-pgid)` so grandchildren die with their progenitor. The host terminal's Ctrl-C reaches the client's own handler — it never becomes stdin bytes — and travels as a `Signal` frame. The client is a relay, never a policy: on SIGINT, SIGTERM, SIGQUIT, or SIGHUP it forwards the signal verbatim and keeps streaming until `Exit`. The child may trap and clean up after, or ignore it; that grace is the child's, not the client's. Job-control signals (SIGTSTP, SIGCONT) are not relayed (accepted limitation). Out-of-range signal numbers and signals for an already-exited child are ignored with a warning log.

Cleanup paths: a client disconnecting before the terminal frame gets its child SIGTERMed by group and reaped while the server keeps serving other connections; the server runs a continuous orphan-reaping loop so no zombies accumulate under the server.

**Blocked by:** 02 (First exec through the socket).

**Status:** ready-for-agent

- [ ] Ctrl-C (SIGINT to the client) interrupts the child promptly; a trapping child observes SIGINT and its cleanup runs; client exits per the child's outcome
- [ ] Signal reaches the whole group: a child that spawned its own subprocess loses both
- [ ] Abrupt client disconnect terminates the child process group; the next connection still works
- [ ] Many sequential children leave no zombies (reaping loop holds)
- [ ] Out-of-range / post-exit signals are ignored with a warning; the connection continues to its normal `Exit`
