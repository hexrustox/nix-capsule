# nix-capsule — overview

nix-capsule runs a Nix devshell inside an OCI container while the user stays in their host shell. Commands typed on the host are forwarded over a Unix socket, executed inside the container with the devshell environment, and stdout/stderr stream back to the host terminal.

## Problem

A devshell built with `nix develop` runs directly on the host: build tools see the host filesystem, host environment, and host network with full user privileges. Containing them normally means entering the container (`docker exec -it …`) and working inside it — losing the host shell, editor integrations, and direnv workflow.

## Solution

Split the devshell in two:

- a **host shell** (e.g. `devShells.default` in the consumer's flake): nearly empty — the `ncap` client, the `ncap-ctl` lifecycle tool, and thin wrapper scripts that route tool names (`cargo`, `rust-analyzer`, …) through the client.
- a **container shell** (a second devshell attr, e.g. `devShells.container`): the real toolchain, never entered directly by the user.

`nix develop` drops the user into the host shell; a shellHook starts the container. The container is a dumb sandbox:

- the host's `/nix` store is mounted read-only,
- the container devshell is **pre-evaluated on the host** (`nix print-dev-env`) into a cached activation script,
- the container's only long-lived process is `ncap-server`, which sources that script and serves the socket.

No Nix evaluation, daemon, or image content is needed inside the container — the image only provides a kernel and userland sandbox.

```
┌─ host ────────────────────────────────────┐   ┌─ container (ncap-<project>) ──────────┐
│ $ nix develop .#                          │   │                                       │
│ host devshell                             │   │  PID 1: ncap-server                   │
│   ├─ ncap (client) ───── Request ─────────┼───┼─▶ unix socket (same-path mount)       │
│   ├─ ncap-ctl (lifecycle)                 │   │   └─ spawns child with devshell env   │
│   └─ cargo (wrapper → `ncap cargo …`)     │   │      (sourced from the env dump)      │
│                                           │   │                                       │
│ ~/.cache/nix-capsule/<p>/env ─────────────┼───┼─▶ mounted ro                          │
│ /nix ─────────────────────────────────────┼───┼─▶ mounted ro                          │
│ $PROJECT_ROOT ────────────────────────────┼───┼─▶ mounted rw, is the workdir          │
└───────────────────────────────────────────┘   └───────────────────────────────────────┘
```

Tool invocation on the host (`ncap cargo build`) sends a request over the shared socket; the server spawns the child with the devshell environment, bridges stdin/stdout/stderr as byte streams, and reports the exit status.

## Goals

- Host-side UX: wrappers make container tools feel local; direnv keeps working with a standard `.envrc`.
- Namespaced, collision-free state per project and per user.
- Env forwarding that captures ad-hoc host variables without wholesale leakage.
- Cheap staleness detection: a fresh shell entry or direnv reload costs a hash check, not a Nix evaluation.
- Signal and exit-code fidelity good enough for scripts, Makefiles, and Ctrl-C workflows.

## Non-goals / accepted limitations

- **No TTY/PTY.** Children run with piped stdio; interactive TUI tools degrade. Accepted for this design.
- **No path translation.** Host paths are valid inside the container because the project root is bind-mounted at the same absolute path; anything not mounted is invisible inside.
- **One command per connection.** No persistent sessions, no multiplexing, no detach mode.
- **Linux only.** Same-path bind mounts, read-only `/nix`, and shared Unix sockets rule out macOS (podman-machine's VM breaks path identity).
- **Dev isolation, not a security boundary against the user.** See Trust model.
- **No Nix evaluation inside the container.** The container environment is a snapshot that changes only on re-init.

## Document map

| Doc | Contents |
| --- | --- |
| `nix.md` | Flake-facing API: options, derived names/paths, staleness, wrappers, env layering |
| `protocol.md` | Wire contract: framing and frame table |
| `server.md` | Server behavior inside the container |
| `client.md` | Client CLI, signal handling, exit codes |
| `ctl.md` | Lifecycle commands, container invocation, mounts, env-var contract |

Shell names used in examples (`default`, `container`) are placeholders — the consumer's flake names both shells; only the linkage between them matters. How the capsule library reaches a flake (overlay, lib function, consuming repo's file layout) is likewise outside this spec's concern: a plain `flake.nix` is all a consumer needs.

## Trust model

- The socket directory is per-user and mode `0700`, under `$XDG_RUNTIME_DIR` (tmpfs by default — it disappears at logout/reboot, so stale sockets don't accumulate). **Anyone who can connect to the socket can execute arbitrary code inside the container**; directory permissions are the only boundary. Accepted: the threat model is accidental host damage, not malicious local users.
- The cache directory is mounted into the container **read-only**: the container must never be able to modify a file the host will later `source` (poisoning).
- The project root is mounted read-write — writing the workspace is the tool's purpose.
- OCI hardening (`--cap-drop=all`, `no-new-privileges`) is available but opt-in.
