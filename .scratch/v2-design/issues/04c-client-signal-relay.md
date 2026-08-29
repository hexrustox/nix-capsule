# 04c: Client signal relay (client side)

**What to build:** Ctrl-C feels local. The client enables tokio's `signal` feature and installs handlers for SIGINT and SIGTERM only; on each event it sends one `Signal { sig }` frame verbatim — the same signal number, one frame per event, repeated signals forwarded repeatedly — then keeps streaming until the terminal frame and exits per the child's outcome. It never puts the terminal in raw mode (the terminal's ISIG line discipline delivers SIGINT to the client itself, never as stdin bytes) and never interprets: a trapping child's cleanup runs; an ignoring child's grace is its own.

SIGQUIT, SIGHUP, SIGTSTP, and SIGCONT keep their default dispositions — killing the client outright drops the connection and 04b's cleanup takes over (accepted limitation). Update `spec/client.md`: the relay section shrinks to SIGINT/SIGTERM, with the defaults noted.

The test harness needs a spawn-style client: `Client::run` blocks, so add a way to spawn the real `ncap`, deliver a signal to it mid-run, and await its output.

**Blocked by:** 04a (the server must deliver `Signal` frames before end-to-end tests can pass).

**Status:** ready-for-agent

- [ ] SIGINT to the client interrupts the child promptly; a trapping child observes SIGINT, its cleanup output streams back, and the client exits with the child's code
- [ ] SIGINT with a non-trapping child → child dies by signal → client exits 130
- [ ] SIGTERM relayed likewise (trapping and non-trapping variants)
- [ ] Repeated SIGINTs forward one frame each (a counting child sees both)
- [ ] Output produced after the signal still streams before the terminal frame
- [ ] `spec/client.md` matches: relay = SIGINT/SIGTERM only
