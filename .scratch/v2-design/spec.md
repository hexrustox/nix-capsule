# nix-capsule v2 — redesign spec

Status: ready-for-agent

## Problem Statement

A `nix develop` shell runs directly on the host: build tools see the host filesystem, environment, and network with full user privileges. The usual fix — running the toolchain in an OCI container — forces the developer *into* the container (`docker exec -it …`), losing the host shell, editor/direnv integrations, and muscle memory.

## Solution

Two devshells per flake, a client/server pair speaking a framed protocol over a shared Unix socket, the container as a dumb sandbox (host `/nix` mounted read-only, container devshell pre-evaluated on the host via `nix print-dev-env`, server as PID 1) — with the following design properties:

- All state namespaced per user and per `project` under XDG dirs (`runtime` / `cache` / `state`), with a stamp-file guard against two checkouts sharing a name.
- `envForward` option: the client resolves a configured list of env vars from the host environment per request, layered under explicit overrides.
- Staleness detection via xxhash64 over `watchFiles` (default `flake.nix` + `flake.lock`): fresh entries cost a hash check, not a Nix evaluation; the dedicated direnv binary and its mtime cache are deleted.
- Socket handling that probes before acting (connectable ⇒ no-op, stale file ⇒ remove) instead of blind clobbering.

Full design lives in `spec/` at the repo root: `overview.md`, `nix.md`, `protocol.md`, `server.md`, `client.md`, `ctl.md`. Those documents are authoritative; this file is the summary.

## User Stories

1. As a flake author, I want to declare a container devshell and get a host shell that routes tools into it, so that my project's toolchain runs contained without changing anyone's workflow.
2. As a flake author, I want to name my devshell attrs anything and consume the capsule from a plain `flake.nix`, so that the capsule doesn't dictate my flake layout.
3. As a developer, I want to run `cargo build` on the host and have it execute inside the container, so that builds are isolated from my host.
4. As a developer, I want stdout/stderr streamed back live, so that long builds show progress as it happens.
5. As a developer, I want stdin forwarded, so that I can pipe data into container commands.
6. As a developer, I want Ctrl-C to interrupt the running child inside the container, so that interrupting feels local.
7. As a developer, I want a second Ctrl-C to force-kill the child, so that I'm never stuck waiting on an unresponsive tool.
8. As a developer, I want a command's child to die when its client dies, so that the container doesn't fill up with orphans.
9. As a developer, I want exit codes preserved, so that scripts and Makefiles chained after wrapped tools behave correctly.
10. As a developer, I want a signal death reported as `128+signal`, so that shell conventions hold.
11. As a direnv user, I want a standard `.envrc` (`watch_file` + `use flake .`) to keep working with no extra direnv tooling, so that my setup stays boring.
12. As a direnv user, I want unchanged-flake reloads to cost a hash check rather than a Nix evaluation, so that `cd`-ing around my repo stays instant.
13. As a developer editing the flake, I want the container environment refreshed on the next shell entry or reload, so that toolchain changes take effect without manual cleanup.
14. As a developer with two checkouts of the same repo, I want per-checkout sockets/containers/caches, so that projects don't collide.
15. As a developer hitting a name collision, I want a clear error telling me to set `project`, so that I can diagnose it in seconds.
16. As a developer, I want ad-hoc host env vars (e.g. `CARGO_HOME`) forwarded per invocation, so that shell configuration reaches container tools without a restart.
17. As a flake author, I want to choose exactly which env vars forward, so that host state doesn't leak wholesale into the container.
18. As a flake author, I want `extraOptions` for extra mounts and env passes, so that caches and credentials can be shared with the container.
19. As a security-minded flake author, I want opt-in hardening flags, so that I can trade capability for confinement.
20. As a developer, I want `ncap-ctl status`/`log`/`enter`, so that I can debug what's happening inside the container.
21. As a developer, I want stale socket files recovered automatically at start, so that a crashed container doesn't wedge shell entry.
22. As a developer, I want `ncap-ctl clean` to wipe all capsule state for the project, so that I can start fresh.
23. As a docker user, I want to select the OCI runtime, so that I'm not forced to install podman.
24. As an editor user, I want my wrapped LSP to stream bidirectionally over one long-lived connection, so that editor integration works through the capsule.
25. As a multi-user machine user, I want the socket in a per-user `0700` runtime dir, so that other users can't reach my container.
26. As a developer, I want no Nix evaluation inside the container, so that container start is fast and independent of the Nix daemon.
27. As a developer without direnv, I want plain `nix develop` to set up and refresh the capsule, so that direnv is optional.

## Implementation Decisions

- **Model:** host shell (`ncap`, `ncap-ctl`, wrappers) + container shell (real toolchain); container = kernel sandbox with `/nix` mounted ro; devshell env pre-evaluated on the host into a cached dump; `ncap-server` is PID 1 after `bash -c "source <env> && exec ncap-server …"`. Image is an option (default `alpine:latest`); the executed binary is always nix-store bash, so the image stays dumb.
- **Flake-facing surface:** `mkShell` (wrapping `mkShellNoCC`) + overlay. Options: `project`, `image`, `devShell`, `watchFiles` (default `["flake.nix" "flake.lock"]`), `envForward`, `wrappers`, `extraOptions`, `harden` (default false, also mounts every `watchFiles` entry read-only), `timeout` (default 10 s), `socketPath`, `containerName`, `preShellHook`/`postShellHook`, `autoStart` (default true). Shell names and consuming-repo file layout are explicitly not part of the contract.
- **Entry contract:** the host devshell's shellHook is the single entry point; `nix develop` and direnv `use flake` are interchangeable triggers and non-direnv use is first-class. The shellHook's guarded `watch_file` emission stays unconditional (inert without direnv, additive with it — no opt-out knob). Without direnv, flake edits take effect on the next shell entry; long-lived sessions never auto-refresh. The client's connect-failure error names the socket path and suggests `ncap-ctl init`.
- **Three binaries:** `ncap` (client), `ncap-server`, `ncap-ctl`.
- **Namespacing:** everything keyed by `project` (default: sanitized basename of the project root). Socket `$XDG_RUNTIME_DIR/nix-capsule/<project>/ncap.sock` (dir 0700, tmpfs ⇒ no stale state across reboots), container `ncap-<project>`, cache `$XDG_CACHE_HOME/nix-capsule/<project>/`, logs `$XDG_STATE_HOME/nix-capsule/<project>/logs/`. `socketPath`/`containerName` override. Cache holds `env`, `hash`, `profile`, `project` (stamp).
- **Stamp guard:** cache records the project root that created it; same project name + different checkout ⇒ hard error with a "set `project`" hint.
- **Staleness:** xxhash64 (non-crypto, pure change detector) over `watchFiles`, checked at `init`. Match ⇒ start-if-not-running. Mismatch ⇒ re-eval `nix print-dev-env` on the host + restart. Mtime-only touches are absorbed by the hash (cheap false alarm). Host-shell-only flake edits also trip it — accepted.
- **Env layering:** devshell dump < `envForward` (client-resolved per request) < wrapper `env` < `-e` overrides. Unset forwarded vars and value-less `KEY` silently omitted. Forwarded values never require a restart; only the layer list itself lives in the flake.
- **Wrappers:** `"name"` shorthand or attrset `{ name, command ? name, env ? [], cwd ? null }`; the attrset mirrors client CLI flags one-to-one and grows with them. Generated as `writeShellScriptBin` PATH shims.
- **Protocol:** 5-byte framing (tag + u32 BE length), JSON struct payloads, raw-byte stdio frames. Frames: `Request 0x01`, `Stdin 0x02`, `Stdout 0x03`, `Stderr 0x04`, `Exit 0x05`, `Error 0x06`, `ServerStopping 0x07`, `Version 0x08` (tag pinned for backward compatibility), `Signal 0x09` (new). One connection per command. `Exit` carries `{code | signal}` explicitly — no information loss; the client still reports shell conventions (child code, `128+signal`).
- **Version handshake stays advisory** (warning only): client and server ship from the same package.
- **Signals:** client traps SIGINT/SIGTERM and forwards them as `Signal` frames (the child is in another PID namespace — the protocol is the only path). First signal = graceful (child may trap/cleanup, client streams until `Exit`). Any second signal before `Exit` = send SIGKILL, ~2 s grace, then client exits 130. Client disconnect without terminal frame ⇒ server SIGTERMs the child and reaps it (no orphans). No detach mode.
- **Exit codes:** child code (u8) / `128+signal` / `1` protocol or transport error / `143` on `ServerStopping` or close-without-terminal / `130` after escalation.
- **Socket hygiene:** `init`/`start` probe before acting — connectable ⇒ no-op, stale file ⇒ remove; the server itself refuses to bind over a live socket and only removes stale ones.
- **Lifecycle:** `init start stop restart enter status log clean completions show-options`. `restart` = non-fatal stop + init. `enter` remains a direct `exec -it` bypass. `clean` wipes project-keyed cache and state. Runtime adapter: podman (default) / docker / explicit path, rootless assumed, Go-template inspect probes. Mounts: `/nix` ro, socket dir rw, `<project root>` rw same-path + workdir, cache ro (poisoning mitigation), log dir rw, `.git` ro when present, every `watchFiles` entry ro when present and `harden = true` (more-specific bind over the rw root; prevents container → host eval-input rewrite); `extraOptions` appended with `$VAR` expansion.
- **Platform:** Linux only; darwin removed from advertised systems.
- **No TTY/PTY ever** — piped stdio only.

## Testing Decisions

- Good tests assert external behavior only: what crosses the socket, what lands on stdout/stderr, what exit code the client reports. Never internal task structure.
- The seam is the **wire protocol** — the highest existing seam, shared by client and server. Integration tests spawn real server and client binaries (tempdir sockets, `CARGO_BIN_EXE_*` env lookups); codec unit tests cover framing round-trips at the same seam's edge.
- Behaviors the suite must exercise: request → exec → streamed stdio; stdin EOF; `Signal` forwarding and escalation; disconnect ⇒ child reaping; `Exit{code|signal}` mapping; stale-vs-live socket probing; staleness hash and name derivation/stamp guard as pure unit tests.
- Lifecycle against a real podman/docker is manual-only (no rootless runtime in CI).
- Detailed test planning is otherwise out of scope for this session.

## Out of Scope

- TTY/PTY allocation and terminal emulation (accepted limitation).
- Path translation between host and container (same-path bind-mount contract only).
- macOS support.
- Nix evaluation inside the container.
- Multiplexing, persistent sessions, detach mode.
- Socket authentication/encryption (per-user dir permissions are the boundary).
- Repo layout, versioning, and release process for this redesign.
- Auto-generating wrappers from the container shell's packages.
- Client-side auto-start of the container on connect failure.

## Further Notes

- `spec/*.md` at the repo root are the authoritative design documents.
- Devshell names in all examples (`default`, `container`) are placeholders.
