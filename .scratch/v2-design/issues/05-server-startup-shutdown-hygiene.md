# 05: Server startup/shutdown hygiene

**What to build:** The server's edges become trustworthy. Startup takes `--socket`, `--log-dir`, `--timeout` (drain grace, seconds). Before binding it probes an existing socket file: connectable means another server owns it — error out without disturbing it; stale means connect fails — remove the file and bind (a crashed container no longer wedges shell entry). Logs land at `<log-dir>/ncap-server-<epoch>.log`.

Shutdown: on SIGTERM/SIGINT (what `ncap-ctl stop` triggers) the server sends `ServerStopping` to every live connection, SIGTERMs every child's process group, drains all connections within `--timeout`, removes the socket file, and exits. Clients react instantly: receiving `ServerStopping` — or a clean socket close without any terminal frame — makes the client bail immediately, stop streaming, and exit 143. Corrupt/garbled traffic remains a transport failure (exit 1), distinct from orderly shutdown.

**Blocked by:** 02 (First exec through the socket).

**Status:** ready-for-agent

- [ ] Stale socket file is removed and the bind succeeds; a live socket refuses startup with an error naming the path, leaving the running server untouched
- [ ] A per-run epoch-stamped log file appears in the log dir
- [ ] SIGTERM to the server ⇒ connected clients get `ServerStopping` and exit 143 immediately, without waiting for their child
- [ ] Children are killed by process group during shutdown; connections drain within the timeout; the socket file is gone after exit
- [ ] Clean close without a terminal frame ⇒ client exits 143; garbled frame data ⇒ client exits 1
- [ ] Shutdown respects `--timeout`: connections finishing inside the grace window complete normally
