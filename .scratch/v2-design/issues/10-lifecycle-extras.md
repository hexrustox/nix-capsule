# 10: Lifecycle extras

**What to build:** The remaining operator commands round out `ncap-ctl` (and one for `ncap`). `enter` is the interactive escape hatch: a direct TTY exec into the container sourcing the env dump — the one deliberate exception to no-TTY — erroring with a suggestion to run `init` when the container is down. `log` opens the newest server log in `$PAGER` (fallback `less -R`). `clean` stops the container if running and deletes everything project-keyed: cache, state (logs included), and runtime dirs — stamp file gone, fresh start guaranteed. `completions <shell>` generate for both binaries. `show-options` prints the `$VAR`-expanded contents of `NCAP_RUN_OPTS`, one arg per line, demanding nothing but that var.

**Blocked by:** 06 (`ncap-ctl` core lifecycle).

**Status:** ready-for-agent

- [ ] `enter` invokes the runtime exec with the expected shape (fake runtime); container down ⇒ error suggesting `ncap-ctl init`
- [ ] `log` picks the newest epoch-stamped file; honors `$PAGER`; falls back to `less -R`
- [ ] `clean` stops a running container and removes all three project-keyed dirs including the stamp; idempotent on already-clean state
- [ ] Completions generate for supported shells for both binaries without error
- [ ] `show-options` expands variables one-per-line; missing var refused by name
