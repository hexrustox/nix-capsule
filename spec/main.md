# nix-capsule Specification

## Overview

`nix-capsule` runs host commands inside a long-lived container (Podman by default) with a Nix devshell environment.

```
ncap cargo build
ncap nil
ncap bash
```

## Components

- **`ncap`** — Host CLI client. See [`client.md`](client.md).
- **`ncap-server`** — Long-lived server inside the container (PID 1). See [`server.md`](server.md).
- **`ncap-ctl`** — Container lifecycle manager on the host. See [`ctl.md`](ctl.md).
- **`ncap-direnv`** — direnv integration. See [`direnv.md`](direnv.md).
- **Nix library (`lib.mkShell`)** — Produces the host devShell with env var configuration and helper scripts.

## Runtime Model

### Startup Flow

1. `ncap-ctl init` runs `nix print-dev-env --profile <profile> <devshell>`, caches the output, and starts/restarts the container.
2. `ncap-ctl start` launches a container running `ncap-server`. The server sources the cached devshell and listens on a Unix socket.
3. `ncap-server` remains alive, accepting client connections.
4. Each `ncap <cmd>` connects to the socket, sends a request, and bridges stdio until the child exits.
5. When the container stops, `ncap-server` receives SIGTERM/SIGINT and notifies active clients before exiting.

### Cache Strategy

- The devshell is evaluated once with `nix print-dev-env` and cached at `.ncap-cache/<devshell>/env`.
- The cache is valid only when `NCAP_CACHE=1` and the env cache file exists on disk.
- `ncap-direnv` sets `NCAP_CACHE=1` when Nix inputs haven't changed. Without direnv, `ncap-ctl init` always regenerates.

### Directories

| Path | Purpose |
|------|---------|
| `.ncap-cache/<devshell>/env` | Cached `nix print-dev-env` output |
| `.ncap-cache/<devshell>/profile` | Nix profile (side effect of `--profile`) |
| `.ncap-cache/direnv-mtimes.json` | direnv file modification time tracking |

### Data Flow

```
Host                              Container
────                              ─────────
ncap ──[Request]──► socket ──► ncap-server
     ◄─[Version]── socket ◄── ncap-server
     ◄─[Stdout]─── socket ◄── child process
     ◄─[Stderr]─── socket ◄── child process
     ──[Stdin]───► socket ──► child process
     ◄─[Exit]──── socket ◄── child process
```

## Wire Protocol

Binary framing protocol over a Unix socket.

### Frame Structure

- **Type** (1 byte)
- **Length** (4 bytes, big-endian)
- **Payload** (variable)

### Frame Types

| Type | Value | Payload | Direction |
|------|-------|---------|-----------|
| Request | 0x01 | JSON (`Request`) | Client → Server |
| Stdin | 0x02 | Raw bytes | Client → Server |
| Stdout | 0x03 | Raw bytes | Server → Client |
| Stderr | 0x04 | Raw bytes | Server → Client |
| Exit | 0x05 | JSON (`Exit`) | Server → Client |
| Error | 0x06 | JSON (`ErrorMessage`) | Server → Client |
| ServerStopping | 0x07 | JSON (`ServerStopping`) | Server → Client |
| Version | 0x08 | JSON (`VersionMsg`) | Server → Client |

### Payload Schemas

**Request** (0x01):
```json
{
  "command": "string",
  "args": ["string", ...],
  "cwd": "string",
  "env": ["KEY=VALUE", ...],
  "version": "0.1.0"
}
```
`version` is optional (absent for legacy clients).

**Exit** (0x05):
```json
{
  "exit_code": 0
}
```

**ErrorMessage** (0x06):
```json
{
  "error": "string",
  "cause": "optional context"
}
```

**ServerStopping** (0x07):
```json
{
  "reason": "optional reason"
}
```

**VersionMsg** (0x08):
```json
{
  "version": "0.1.0"
}
```

### Request Flow

1. Client connects, sends `Request`.
2. Server responds with `Version`, spawns the child.
3. Bidirectional streaming: `Stdin` frames from client, `Stdout`/`Stderr` from server.
4. Server sends `Exit` on child exit, or `Error` on spawn failure.
5. Connection closes after `Exit` or `Error`.

### Shutdown Flow

1. Server receives SIGTERM/SIGINT.
2. Broadcasts `ServerStopping` to all active connections.
3. Stops accepting new connections and exits.
4. Clients receiving `ServerStopping` exit with code 143.

## Nix Library (`lib.mkShell`)

### Signature

```
mkShell {
  image          # Container image (required)
  devShell       # Nix devshell attribute path (e.g. "container" or ".#container")
  socketPath     # Unix socket path
  logDir         # Server log directory (default: socket parent directory)
  containerName  # Container name
  runtime        # Container runtime (default: "podman")
  extraOptions   # Additional runtime options (default: [])
  extraPackages  # Additional packages in devShell (default: [])
  harden         # Enable hardening flags (default: false)
  autoStart      # Run ncap-ctl init in shellHook (default: true)
  timeout        # Server drain timeout in seconds (default: 10)
  wrappers       # Commands to wrap with ncap (default: [])
  preShellHook   # Shell code before auto-start (default: "")
  postShellHook  # Shell code after auto-start (default: "")
}
```

### devShell Resolution

- If `devShell` starts with `.` or `/`, use as-is.
- Otherwise, prefix with `.#`.

### Wrappers

Each entry in `wrappers` creates a `writeShellScriptBin` in the devShell's `packages`. Entries may be:
- A string: `"cargo"` → script `cargo` runs `ncap cargo "$@"`.
- An attrset `{ name, value }`: script `name` runs `ncap value "$@"`.

### Environment Variables

| Variable | Value |
|----------|-------|
| `NCAP_SOCKET` | `socketPath` |
| `NCAP_LOG_DIR` | `logDir` or `dirOf socketPath` |
| `NCAP_DEVSHELL` | Resolved devShell path |
| `NCAP_CONTAINER` | `containerName` |
| `NCAP_IMAGE` | `image` |
| `NCAP_RUNTIME` | `runtime` |
| `NCAP_RUN_OPTS` | JSON array of runtime options |
| `NCAP_TIMEOUT` | `timeout` (seconds) |
| `NCAP_SERVER` | Path to `ncap-server` |
| `NCAP_NIX` | Path to `nix` |
| `NCAP_BASH` | Path to `bash` |

### Runtime Configuration

#### Hardcoded Defaults

`ncap-ctl` applies these container arguments (before user-supplied options):

| Argument | Purpose |
|----------|---------|
| `--replace` | Replace existing container with same name |
| `--name <containerName>` | Container name |
| `-v /nix:/nix:ro` | Nix store (read-only) |
| `-v $socketDir:$socketDir` | Socket directory (read-write) |
| `-v $PROJECT_ROOT:$PROJECT_ROOT` | Project root (read-write) |
| `-w $PROJECT_ROOT` | Working directory |
| `-v $PROJECT_ROOT/.ncap-cache:$PROJECT_ROOT/.ncap-cache:ro` | Devshell cache (read-only) |
| `-v $PROJECT_ROOT/.git:$PROJECT_ROOT/.git:ro` | Git metadata (read-only, only if `.git/` exists) |

#### User Options (`NCAP_RUN_OPTS`)

- When `harden=true`: `--cap-drop=all`, `--security-opt=no-new-privileges`
- Any `extraOptions` appended

Template variables (`$PROJECT_ROOT`, `${VAR}`) are expanded at runtime.

### shellHook

```
${preShellHook}
${optionalString autoStart "ncap-ctl init"}
${postShellHook}
```

## Execution Semantics

- Arguments, stdio, exit code, and working directory pass through transparently.
- Long-lived processes (LSP servers) are supported — the connection stays open until the child exits or the client disconnects.
- Multiple `ncap` invocations may run concurrently against the same server. Each invocation is independent.
- The server handles SIGTERM/SIGINT as PID 1, notifies active clients via `ServerStopping` frames, drains connections with a configurable timeout, then exits.
