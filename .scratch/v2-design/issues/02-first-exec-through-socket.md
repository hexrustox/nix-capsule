# 02: First exec through the socket

**What to build:** The tracer bullet for the whole product: `ncap [--socket PATH] [--cwd PATH] [--] COMMAND [ARGS…]` connects to a running `ncap-server` over a Unix socket, sends `Request`, and the named command executes inside the server's environment with live streamed output — stdout/stderr appear as they are produced, piped stdin reaches the child, and the client exits with the child's status.

Details that must hold on this first path: the client's current directory is the default `cwd`; request env entries are applied by the server over its inherited environment; the server replies with an advisory `Version` (mismatch or missing version warns, never rejects); the first frame must be `Request`, anything else is an `Error` and close. Spawn failure with ENOENT/EACCES returns as terminal `Exit { code: 127 | 126 }` with the client synthesizing `ncap: <command>: command not found` / `permission denied` on stderr; any other spawn failure, decode failure, or transport failure is terminal `Error` ⇒ client exit 1. Connect failure prints a static hint naming the socket path and suggesting `ncap-ctl init` — never an implicit auto-start. A child death by signal reports exit `128 + signal`. An unknowable status (`Exit { null, null }`) warns on stderr and exits 1.

All verification via integration tests spawning the real binaries (`CARGO_BIN_EXE_*` lookups) against tempdir sockets.

**Blocked by:** 01 (Cargo skeleton + wire-protocol codec).

**Status:** ready-for-agent

- [ ] Request → exec → stdout/stderr stream live → client exit equals child's code truncated to u8
- [ ] Piped stdin reaches the child; closing the client's write half gives the child EOF
- [ ] Child killed by signal ⇒ client exits `128 + signal`
- [ ] ENOENT/EACCES spawn failures yield synthesized stderr lines and exits 127/126, with no `Error` frame preceding them
- [ ] Other spawn failure ⇒ terminal `Error`, client exits 1; non-`Request` first frame ⇒ `Error` and close
- [ ] `Exit { null, null }` ⇒ warning on stderr, exit 1
- [ ] Version mismatch (or absent version) warns on stderr but the command still succeeds
- [ ] Connect failure names the socket path and suggests `ncap-ctl init`
