# 06: `ncap-ctl` core lifecycle

**What to build:** Shell entry gets its lifecycle brain. `ncap-ctl init/start/stop/restart/status` run entirely off the `NCAP_*` env-var contract — each command demands exactly the vars it uses and refuses naming the missing one (`completions`/`show-options` exempt). Configuration includes project-name derivation from the project root (non-alphanumeric runs collapse to `-`, edges stripped, empty result is a hard error telling you to set `project`) and the XDG layout with its `$TMPDIR` fallback for the runtime dir (created mode 0700).

Safety rails: the stamp guard reads the cached project-root stamp first — same name owned by a different checkout is a hard error with a "set `project`" hint; absent means write it. The freshness hash is xxhash64 over one record per watchFiles entry, sorted by relative path: `(relative path`, NUL, existence flag, NUL, contents-or-empty)`; missing files contribute their absence so appearing/disappearing flips freshness; mtime-only touches do not; cached as lowercase hex with no trailing newline.

The flow: `init` = stamp guard → probe liveness via runtime inspect (`State.Running`; the server is the container's init process, so no socket polling) → running + fresh ⇒ done with no Nix evaluation → running + stale ⇒ re-eval + restart → down ⇒ ensure cache (`nix print-dev-env` into the cache when stale or missing, profile history pruned) + start. `start` launches detached, polls to `State.Running` within the deadline, and recovers from a concurrent-start race ("name in use": re-inspect — running ⇒ success, dead ⇒ remove container and start once more); never reaching Running fails loudly with the inspect state and the newest log tail. `stop` is idempotent; `restart` is non-fatal stop then `init`. `status` reports container running / socket connectable / cache fresh-stale-missing. The runtime adapter honors `NCAP_RUNTIME`: podman default, docker, or an absolute path.

All verification against a scripted fake runtime standing in for podman/docker (stub executable + stub `nix`, call-counting for eval avoidance). Real-runtime proof is ticket 07's job.

**Blocked by:** 01 (Cargo skeleton + wire-protocol codec).

**Status:** ready-for-agent

- [ ] Each command refuses with an error naming exactly the missing `NCAP_*` var
- [ ] Name sanitization rules hold, including the empty-result hard error; overrides via the dedicated options are honored
- [ ] Stamp guard: different root under same name ⇒ error with hint; same root ⇒ pass; absent ⇒ written
- [ ] Digest vectors: entry order irrelevant (sorted), content change flips, file appearing/disappearing flips, mtime-only touch doesn't; hex format stable
- [ ] `init` on fresh+running performs zero Nix evaluations (call-counted); stale triggers re-eval + restart; down triggers ensure-cache + start
- [ ] Readiness deadline honored; failure output includes inspect state and newest log tail
- [ ] Concurrent-start race resolves both ways (peer running ⇒ success; peer dead ⇒ rm + single retry)
- [ ] `stop` idempotent; `restart` tolerates a stopped container; `status` covers all three dimensions
- [ ] Runtime selection follows `NCAP_RUNTIME` (podman default, docker, explicit path)
