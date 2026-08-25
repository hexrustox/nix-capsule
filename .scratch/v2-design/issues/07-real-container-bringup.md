# 07: Real container bring-up

**What to build:** The full start invocation runs against a genuine rootless OCI runtime. The launch assembles the default mount set — host `/nix` read-only, socket dir at its identical path read-write, project root at its identical path read-write as the working directory, cache dir read-only (the container must never be able to poison files the host later sources), log dir read-write, `.git` read-only when it exists — then runs the image detached with nix-store bash sourcing the env dump and exec'ing `ncap-server` so it becomes PID 1.

The argument surface is locked by fake-runtime integration tests (same harness as ticket 06). What only a human can verify is the real thing end-to-end: CI has no rootless runtime, so this ticket carries a short manual checklist instead of automated proofs. Done means: enter the host devshell of a consuming flake and run wrapped tools that execute inside the container with the container shell's environment.

**Blocked by:** 05 (Server startup/shutdown hygiene), 06 (`ncap-ctl` core lifecycle).

**Status:** ready-for-agent

- [ ] Fake-runtime test asserts the exact default mount set and the launch command shape (source dump && exec server with socket/log/timeout flags)
- [ ] Manual: `nix develop` entry boots the container; a wrapped command executes inside with the container shell's toolchain resolving via plain PATH
- [ ] Manual: stdout/stderr stream live through a real long-running command; exit codes propagate to the host shell
- [ ] Manual: wedged start (bad image) fails loudly with inspect state + log tail rather than hanging
- [ ] Manual: two concurrent `init`s don't wedge or double-start (race recovery holds for real)
- [ ] Manual: after killing the container, next start recovers any stale socket file cleanly
- [ ] Manual: `.git` mount present read-only in a git repo; absent without error outside one
