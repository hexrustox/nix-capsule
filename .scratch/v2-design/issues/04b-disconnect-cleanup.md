# 04b: Disconnect cleanup (server side)

**What to build:** A client vanishing before the terminal frame must not orphan its child. The only disconnect signal the server gets is a failed send — EPIPE while pumping output toward the client; on it, the server SIGTERMs the child's process group, reaps the child, and keeps serving other connections. No KILL escalation — the grace after SIGTERM is the child's, not the server's: the connection task simply awaits `child.wait()`, however long that takes. Read-side EOF keeps meaning stdin EOF (write-half close), never a disconnect.

Today's disconnect path calls `child.start_kill()` (SIGKILL, process-only) and drops the handle; replace it with the group TERM and wait.

Accepted limitation (locked design decision): a silent child — no output, stdin already EOF'd — of a vanished client runs to completion; nothing observable triggers detection. The disconnect tests therefore use children that produce output.

All verification via integration tests against the real binaries with raw wire connections.

**Blocked by:** 04a (needs the group-kill helper).

**Status:** ready-for-agent

- [ ] Abrupt full close while a child producing periodic output runs → the whole group is TERMed within ~2s; the next connection still works
- [ ] A child that spawned its own subprocess loses both on disconnect
- [ ] The disconnect-TERMed child is reaped — no zombie left under the server
- [ ] Regression guard: write-half-only close (stdin EOF) still lets `sh -c 'cat; echo done'` finish — EOF never kills
- [ ] A child trapping and ignoring SIGTERM holds only its own reaper task; other connections are unaffected
- [ ] The accepted limitation is noted as a comment in `server.rs` next to the disconnect path
