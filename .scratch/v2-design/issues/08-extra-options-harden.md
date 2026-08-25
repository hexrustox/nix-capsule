# 08: `extraOptions` + `harden`

**What to build:** Consumers can share caches/credentials and trade capability for confinement. `extraOptions` args are appended after the default mounts, with `$VAR`/`${VAR}` expanded at launch against `ncap-ctl`'s own environment — expansion happens once, with no word-splitting afterwards, and a referenced unset variable is a launch error naming it. This is what makes `$CARGO_HOME:$CARGO_HOME`-style mounts work.

`harden` prepends `--cap-drop=all --security-opt=no-new-privileges` and bind-mounts every present watchFiles entry read-only over the read-write project-root mount — the more-specific mount wins, so eval inputs stay immutable even though the parent directory is writable; absent entries are skipped silently. Default remains off because capability drops occasionally break dev tooling.

Argument surface verified by fake-runtime integration tests; the real-runtime behavioral proof joins ticket 07's manual checklist territory since CI has no rootless runtime.

**Blocked by:** 07 (Real container bring-up).

**Status:** ready-for-agent

- [ ] Fake-runtime tests assert expansion results, append-after-defaults ordering, and literal passthrough without word splitting
- [ ] Unset referenced variable ⇒ launch error naming it, before any container is started
- [ ] `harden` adds both security flags and ro-mounts each present watchFiles entry; missing entries skipped
- [ ] `harden` off (default) emits neither the flags nor the extra mounts
- [ ] Manual: an `extraOptions` cache mount is genuinely shared (host write visible inside the container)
- [ ] Manual: hardened container cannot rewrite watched files; basic tooling still runs
