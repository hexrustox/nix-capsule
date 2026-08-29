# 04d: Orphan reaping (no zombies)

**What to build:** Many sequential children — normal exits, signal deaths, disconnect-TERMed groups — leave no zombie processes under the server. Verify first: tokio's process driver already reaps both awaited and dropped children through its internal waitpid loop, so the outcome may hold with zero code. Only add an explicit reaper if the test proves otherwise. Do NOT call `waitpid(-1)` ad hoc via libc alongside tokio — it steals statuses tokio's reaper is waiting on.

One integration test against the real server: run a mix of children sequentially through one server instance, then scan `/proc` for processes whose state is `Z` and whose PPID is the server's pid.

**Blocked by:** 04b (disconnect-TERMed children are part of the mix).

**Status:** ready-for-agent

- [ ] ~30 sequential children (`exit 0`, `kill -9 $$`, disconnect-TERMed sleepers) through one server → the `/proc` scan finds no zombie with PPID = server pid
