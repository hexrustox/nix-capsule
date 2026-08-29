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
- the container devshell is **pre-evaluated on the host** (`nix print-dev-env`) into a cached env dump,
- the container's only long-lived process is `ncap-server`, which sources that dump and serves the socket.

No Nix evaluation, daemon, or image content is needed inside the container — the image only provides a kernel and userland sandbox.

The **project root** is the workspace directory nix-capsule operates on: the git toplevel of the consumer's checkout, falling back to the current directory outside a git repo. It anchors everything else — the project name (sanitized basename), the workspace mount inside the container, and the workdir of executed commands. It is not itself an environment variable; `ncap-ctl` receives it via `NCAP_PROJECT_ROOT` (`ctl.md`).

```
┌─ host ────────────────────────────────────┐   ┌─ container (ncap-<project>) ──────────┐
│ $ nix develop .#                          │   │                                       │
│ host devshell                             │   │  init: ncap-server                    │
│   ├─ ncap (client) ───── Request ─────────┼───┼─▶ unix socket (same-path mount)       │
│   ├─ ncap-ctl (lifecycle)                 │   │   └─ spawns child with devshell env   │
│   └─ cargo (wrapper → `ncap cargo …`)     │   │      (sourced from the env dump)      │
│                                           │   │                                       │
│ ~/.cache/nix-capsule/<p>/env ─────────────┼───┼─▶ mounted ro                          │
│ /nix ─────────────────────────────────────┼───┼─▶ mounted ro                          │
│ project root ─────────────────────────────┼───┼─▶ mounted rw, is the workdir          │
└───────────────────────────────────────────┘   └───────────────────────────────────────┘
```

Tool invocation on the host (`ncap cargo build`) sends a request over the shared socket; the server spawns the child with the devshell environment, bridges stdin/stdout/stderr as byte streams, and reports the exit status.

## Goals

- Host-side UX: wrappers make container tools feel local; the host devshell's shellHook is the entry point — plain `nix develop` works with no direnv installed, and direnv keeps working with a standard `.envrc`.
- State keyed per project and per user — separate checkouts never share it.
- Env forwarding that captures ad-hoc host variables without wholesale leakage.
- Cheap freshness checks: a fresh shell entry or direnv reload costs a hash check, not a Nix evaluation.
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
| `nix.md` | Flake-facing API: options, derived names/paths, freshness, wrappers, env layering |
| `protocol.md` | Wire contract: framing and frame table |
| `server.md` | Server behavior inside the container |
| `client.md` | Client CLI, signal handling, exit codes |
| `ctl.md` | Lifecycle commands, container invocation, mounts, env-var contract |

Shell names used in examples (`default`, `container`) are placeholders — the consumer's flake names both shells; only the linkage between them matters. How the `nix-capsule` library reaches a flake (overlay, lib function, consuming repo's file layout) is likewise outside this spec's concern: a plain `flake.nix` is all a consumer needs.

## Trust model

- The socket directory is per-user and mode `0700`, under `$XDG_RUNTIME_DIR` (tmpfs by default — it disappears at logout/reboot, so stale sockets don't accumulate). **Anyone who can connect to the socket can execute arbitrary code inside the container**; directory permissions are the only boundary. Accepted: the threat model is accidental host damage, not malicious local users.
- The cache directory is mounted into the container **read-only**: the container must never be able to modify a file the host will later `source` (poisoning). Under `harden`, the same invariant extends to every `watchFiles` entry — each is bind-mounted read-only over the read-write project root so a contained process cannot rewrite watched files whose change triggers `nix print-dev-env` on the host.
- The project root is mounted read-write — writing the workspace is the tool's purpose.
- OCI hardening (`--cap-drop=all`, `no-new-privileges`) is available but opt-in (see above for the additional `watchFiles` mounts under `harden`).
