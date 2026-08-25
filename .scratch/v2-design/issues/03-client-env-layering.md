# 03: Client env layering

**What to build:** Ad-hoc host environment reaches container children per invocation, with no restarts. On the client: `--env KEY=VALUE` sets an explicit override; bare `--env KEY` copies that variable from the client's own environment if set, silently omitted otherwise; before sending `Request`, every name in `NCAP_ENV_FORWARD` (a JSON array of variable names) is resolved from the client's current environment into the request env. The merge is later-wins with deduplication across all `--env` flags and forwarded names; unset entries are silently omitted — nothing errors.

The point of the slice: a changed host value takes effect on the very next command without touching the server, while the server keeps applying whatever list arrives over its inherited environment (already built in ticket 02). Wrapper-supplied env rides the same pre-filled-flag path, so this completes env layers 2–4 end-to-end at the client.

**Blocked by:** 02 (First exec through the socket).

**Status:** ready-for-agent

- [ ] `-e KEY=VALUE` is visible in the child's environment
- [ ] Forwarded name resolved fresh each invocation: changing the host value between two invocations changes what the child sees, with no server restart
- [ ] Bare `KEY` copies when set on the host; is silently omitted when unset
- [ ] A name in `NCAP_ENV_FORWARD` that is unset on the host is silently omitted
- [ ] Duplicate keys resolve later-wins across multiple `--env` flags
- [ ] Merged list arrives deduplicated in the request env
