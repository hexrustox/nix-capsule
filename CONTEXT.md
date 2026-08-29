# nix-capsule

Shared vocabulary for running a Nix devshell inside an OCI container while the user stays in their host shell: commands travel over a per-project Unix socket from thin host shims into a dumb sandbox whose server spawns them with a pre-evaluated devshell environment.

## Language

Note: "devshell" alone is ambiguous here — always say **Host shell** or **Container shell**.

### Sides

**Host shell**:
The nearly empty devshell the consumer enters (`nix develop`); its tools exist in the container and are reached through wrappers.
_Avoid_: default shell, outer shell, login shell

**Container shell**:
The devshell holding the real toolchain (typically mkShell option `devShell`); evaluated once on the host into the env dump, never entered directly.
_Avoid_: inner shell, toolchain shell, target shell

**Container**:
The OCI sandbox backing one project (named `ncap-<project>`); deliberately dumb — no Nix evaluation or daemon runs inside it.
_Avoid_: capsule, sandbox, VM

**Project root**:
The git toplevel of the consumer's checkout, falling back to the current directory outside git; anchors the project name, the workspace mount, and every command's workdir.
_Avoid_: workspace root, repo root, project dir

### Components

**Client**:
The host binary `ncap`; sends one command per connection to the server and streams stdio back to the terminal.
_Avoid_: frontend, runner

**Server**:
The binary `ncap-server`; PID 1 inside the container — sources the env dump at startup and serves the socket.
_Avoid_: daemon, agent

**Ctl**:
The host binary `ncap-ctl`; owns the project's lifecycle (init, start, stop, status, clean).
_Avoid_: controller, manager

**Wrapper**:
A PATH shim in the host shell shadowing a real tool name, routing through the Client with fixed arguments baked in.
_Avoid_: shim, alias, stub

**Runtime adapter**:
The interchangeable OCI runtime — podman or docker — behind all container run/inspect/stop operations.
_Avoid_: engine, backend

### State

**Project name**:
The identifier derived from the project root (sanitized basename); keys the socket, container, cache, and log paths.
_Avoid_: namespace, slug, bare "project"

**Socket**:
The per-project Unix socket appearing at the same absolute path on host and container — the sole channel between Client and Server.

**Cache**:
The per-project store of derived artifacts kept outside the project tree; chiefly the env dump, the freshness hash, and the stamp file.
_Avoid_: build cache, ncap-cache (as prose)

**Env dump**:
The cached `nix print-dev-env` snapshot of the Container shell, sourced by the Server so children inherit the toolchain environment.
_Avoid_: activation script, devshell dump, print-dev-env output

**Stamp guard**:
The rule binding a project name to exactly one project root, enforced by a stamp file in the Cache.
_Avoid_: collision guard, ownership stamp

**Freshness**:
Whether the cached env dump still matches the watched files' contents — decided by hash comparison, never by Nix evaluation; states are fresh / stale / missing.
_Avoid_: staleness check, cache invalidation

**Watched files**:
Project-root-relative files (mkShell option `watchFiles`) deciding freshness, emitted into direnv's watch set, and bind-mounted read-only under `harden`.
_Avoid_: eval inputs, watch set

**Harden**:
The opt-in posture adding capability drop, no-new-privileges, and read-only bindings for watched files atop the read-write workspace mount.
_Avoid_: strict mode, secure mode

### Execution model

**Connection**:
One client connection executes exactly one command and spawns one Child; there are no persistent sessions or multiplexing.

**Child**:
The process the Server spawns per request, placed in its own process group so every descendant dies with it.
_Avoid_: job, task, worker

**Signal relay**:
The rule that the Client forwards every accepted host signal verbatim as a `Signal` frame and the Server delivers it to the Child's process group; neither side interprets or escalates.
_Avoid_: escalation, signal policy

**Terminal frame**:
The single frame — `Exit` or `Error` — that ends every connection; nothing arrives after it.
_Avoid_: final frame, exit message

**Drain grace**:
The bounded time a stopping Server grants live connections to finish before they're dropped with container teardown.
_Avoid_: drain timeout, --timeout (as prose)

### Environment layering

**Environment layering**:
The fixed precedence for what a Child sees, lowest to highest: env dump, forwarded host variables, wrapper variables, CLI flags — highest layer defining a key wins.
_Avoid_: env merge, override stack, devshell dump (for this layer)

**Forwarded variable**:
A variable name resolved by the Client from the host environment on every request, so new host values apply immediately; changing which names are forwarded edits the flake and trips freshness.
_Avoid_: pass-through env, injected var
