# 04a: Process groups + Signal delivery (server side)

**What to build:** Every child the server spawns sits in its own process group — `tokio::process::Command::process_group(0)`, making the child its own group leader — and `Signal` frames from the client reach that whole group via `kill(-pgid, sig)`, so grandchildren die with their progenitor. The server is a relay, not a policy: the frame's signal number is forwarded verbatim, whatever it is. A `kill` failure — the group already gone (ESRCH, child exited) or an invalid number (EINVAL, out of range) — is one warning line on the server's stderr, never an `Error` frame, and the connection continues to its normal terminal frame.

The `bridge` loop currently ignores `Signal` frames (the arm is marked for ticket 04); replace it. Add the `libc` dependency for `kill`. Update `spec/server.md`: the process-group wording becomes `process_group(0)` (not `setsid`), and the Signal bullet becomes "forward any number verbatim; kill failures warn and the connection continues".

All verification via integration tests speaking raw wire frames against the real `ncap-server` (the `Server::raw()` helper), plus one unit test for the kill helper's error mapping.

**Blocked by:** 02 (First exec through the socket).

**Status:** ready-for-agent

- [ ] Child spawns with `process_group(0)`: a child observes itself as its own process-group leader
- [ ] `Signal { TERM }` to a trapping `sh` runs the trap; the connection ends with the child's own `Exit`
- [ ] `Signal { INT }` reaches the whole group: a child that spawned its own subprocess loses both
- [ ] A signal for an already-exited group maps ESRCH to a warning (unit test over the kill helper with a reaped pgid)
- [ ] An out-of-range number (e.g. 200) is forwarded, hits EINVAL, warns only, and the connection continues to its normal `Exit`
- [ ] `spec/server.md` matches the implementation
