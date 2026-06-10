# Security Model

## Trust Boundary

The Unix socket file is the sole trust boundary. Any process with write access to the socket can execute arbitrary commands inside the container. The socket directory is assumed exclusive to the container and free of sensitive data.

## Isolation Layers

### Container Runtime

The container runtime (podman/docker) is the primary isolation mechanism. Hardening options (`--cap-drop=all`, `--security-opt=no-new-privileges`) are available via the `harden` flag but are not enabled by default.

### Client-Side Safety

The client (`ncap`) never executes the requested command. It serializes the request, sends it over the socket, and bridges raw stdio bytes. No command parsing, shell expansion, or execution occurs on the host.

### Server-Side Execution

The server (`ncap-server`) spawns child processes inside the container with piped stdio (no TTY). It never exec's the child — the server remains as PID 1 managing I/O bridges and graceful shutdown. The child inherits the devshell environment, not the host environment.

## Data in Transit

All communication occurs over a Unix socket in the exclusive socket directory. No encryption or authentication beyond filesystem permissions.

## Bind Mounts

| Mount | Permission | Risk |
|-------|-----------|------|
| `/nix:/nix` | `ro` | Container cannot modify host nix store |
| `$PROJECT_ROOT:$PROJECT_ROOT` | `rw` | Container has write access to project files |
| `$socketDir:$socketDir` | `rw` | Required for socket communication |
| `$PROJECT_ROOT/.ncap-cache:$PROJECT_ROOT/.ncap-cache` | `ro` | Prevents compromised container from poisoning cached env |
| `$PROJECT_ROOT/.git:$PROJECT_ROOT/.git` | `ro` | Prevents tampering with git history (only if `.git/` exists) |

## Host Environment

### Environment Variables

`NCAP_*` variables configure the capsule on the host and are consumed by `ncap-ctl`. They are not passed into the container. The server receives its configuration via command-line arguments (`--socket`, `--log-dir`, `--timeout`). The devshell environment that the server sources inside the container comes from the cached `.ncap-cache/<devshell>/env` file, not from `NCAP_*` variables.

### Cached State

`.ncap-cache/<devshell>/env` is the output of `nix print-dev-env` — it captures the evaluated devshell environment, including any secrets that exist in the flake or devshell definition. That exposure is inherent to where secrets are defined, not specific to the cache.

The primary threat was that `.ncap-cache/` lived inside `$PROJECT_ROOT`, which was bind-mounted `rw` into the container — a compromised container process could overwrite `.ncap-cache/<devshell>/env` with arbitrary content. When `ncap-ctl init` next ran, sourcing that file on the host granted code execution. This is now mitigated by mounting `.ncap-cache` as `ro`.

## Threat Scenarios

| Threat | Mitigation |
|--------|-----------|
| Untrusted process writes to socket | Socket dir assumed exclusive — compromise of this dir is game-over |
| Container escape | Relies on the container runtime; `harden` mode reduces attack surface |
| Host nix store modified by container | Mounted `ro` — container cannot write to host `/nix` |
| Container modifies project files | Bind-mounted `rw` — accepted risk for development workflow |
| Container writes to cached env | `.ncap-cache` is `ro` — a compromised container can no longer poison the cached env for host code execution |
| Command lines exposed in logs | Logs contain executed commands; log dir is on the host filesystem |

## Assumptions

- The container runtime is correctly configured and not compromised.
- The socket directory is exclusively owned by the capsule and contains no sensitive data.
- The host and container share a nix store; the container only reads it.
- Anyone who can connect to the socket can execute arbitrary code inside the container.
