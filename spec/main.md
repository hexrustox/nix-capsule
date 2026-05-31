# nix-capsule Specification

## 1. Overview

`nix-capsule` provides explicit host commands that execute inside a long-lived container (Podman by default), within a real Nix devshell already entered inside that container.

Example usage:

```
ncap cargo build
ncap nil
ncap bash
```

Goals:

- Containerized execution of dev tools
- Correct Nix devshell environment fidelity
- Low per-command overhead
- Host/editor access to tools available only inside the container

## 2. Components

- **`ncap`**: Host CLI client. Connects to a Unix socket, sends execution requests, bridges stdio between the terminal and the server.
- **`ncap-server`**: Long-lived server inside the container (PID 1). Listens on the Unix socket, spawns requested commands as child processes, bridges their stdio back to the client. Writes structured logs to timestamped files. Handles SIGTERM/SIGINT directly for graceful shutdown — notifies active clients before exiting.
- **`ncap-ctl`**: Container lifecycle manager on the host. Subcommands: `init`, `start`, `stop`, `restart`, `enter`, `show-options`. Uses `nix print-dev-env` to cache the devshell environment, then manages the container lifecycle.
- **`ncap-direnv`**: direnv integration binary. Compares Nix file modification times against a stored state to determine if the cached devshell environment is still valid. Outputs a shell script fragment for direnv to eval, defining a `use_flake()` function and setting the `NCAP_CACHE` environment variable.
- **Nix library (`lib.mkShell`)**: Produces the host-facing devShell with environment variable configuration, wrapper commands, and a `shellHook` that triggers `ncap-ctl init`.

## 3. Runtime Model

### Startup Flow

1. `ncap-ctl init` runs `nix print-dev-env --profile <profile> <devshell>`, caches the output to `.ncap-cache/<devshell>/env`. If the container is already running, it is restarted.
2. `ncap-ctl start` launches a container running `ncap-server` as the entrypoint (PID 1). The server sources the cached devshell environment and listens on the Unix socket. The container binds `/nix:/nix:ro`, the project root, and the socket directory.
4. `ncap-server` remains alive, listening on a Unix socket.
5. Each `ncap <cmd>` invocation opens a new connection to the socket, sends a request, and bridges stdio until the child exits.
6. When the container stops, `ncap-server` receives SIGTERM/SIGINT directly and notifies active clients before exiting.

### Cache Strategy

- The devshell environment is evaluated once with `nix print-dev-env` and cached to disk at `.ncap-cache/<devshell>/env`.
- `ncap-ctl start` sources this cached file to enter the devshell without re-running `nix print-dev-env` on every start.
- Cache invalidation in `ncap-ctl init`: the cache is considered valid only when `NCAP_CACHE=1` AND the env cache file exists on disk. `ncap-direnv` sets `NCAP_CACHE=1` when the Nix devshell inputs haven't changed. Without direnv integration, `NCAP_CACHE` is never set to `1`, so `ncap-ctl init` always regenerates the cache.

### Auto-Start

When `autoStart` is enabled (default), the devShell's `shellHook` runs `ncap-ctl init` before each shell session, which caches the environment and starts the container if needed.

### Directories

| Path | Purpose |
|------|---------|
| `.ncap-cache/` | Cache root directory |
| `.ncap-cache/<devshell>/env` | Cached `nix print-dev-env` output |
| `.ncap-cache/<devshell>/profile` | Nix profile linked by `--profile` |
| `.ncap-cache/direnv-mtimes.json` | direnv file modification time tracking |

### Data Flow

```
Host                              Container
────                              ─────────
ncap ──[Request]──► socket ──► ncap-server
     ◄─[Stdout]─── socket ◄── child process
     ◄─[Stderr]─── socket ◄── child process
     ──[Stdin]───► socket ──► child process
     ◄─[Exit]──── socket ◄── child process
```

One connection equals one command. The server handles multiple concurrent connections by spawning a task per connection.

## 4. Wire Protocol

Communication uses a custom binary framing protocol over a Unix socket.

### Frame Structure

Each frame consists of:

- **Type** (1 byte): Identifies the frame kind
- **Length** (4 bytes, big-endian): Payload length in bytes
- **Payload** (variable): Frame-specific data

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

### Payload Schemas

**Request** (0x01):
```json
{
  "command": "string",
  "args": ["string", ...],
  "cwd": "string",
  "env": ["KEY=VALUE", ...]
}
```

**Exit** (0x05):
```json
{
  "exit_code": 0
}
```
`exit_code` is an `i32` integer.

**ErrorMessage** (0x06):
```json
{
  "error": "string",
  "cause": null
}
```
`cause` is an optional string providing additional context (e.g., the command and working directory that failed).

**ServerStopping** (0x07):
```json
{
  "reason": null
}
```
`reason` is an optional string.

### Request Flow

1. Client connects to the Unix socket.
2. Client sends a single `Request` frame.
3. Server spawns the child process.
4. Bidirectional streaming begins:
   - Client sends `Stdin` frames as data arrives on its stdin.
   - Server sends `Stdout` and `Stderr` frames as the child produces output.
5. When the child exits, the server sends an `Exit` frame with the exit code.
6. If the child fails to spawn or an error occurs, the server sends an `Error` frame instead.
7. The connection closes after `Exit` or `Error`.

### Shutdown Flow

1. `ncap-server` receives SIGTERM or SIGINT directly (as PID 1 when the container stops).
2. Server broadcasts a `ServerStopping` frame to all active client connections.
3. Server stops accepting new connections and exits.
4. Clients receiving `ServerStopping` print the reason if any and exit with code 143 (128 + SIGTERM).

## 5. Client (`ncap`)

### CLI Interface

```
ncap [OPTIONS] <COMMAND> [ARGS...]
```

Options:

- `--socket <PATH>` / `-s <PATH>`: Unix socket path. Reads from `NCAP_SOCKET` env var if not provided.
- `--env <KEY=VALUE>` / `-e <KEY=VALUE>`: Environment variable overrides. May be specified multiple times. The value may be `KEY=VALUE` (sent as-is) or bare `KEY` (looked up from the host process environment — if found, sent as `KEY=value`; if not found, silently skipped).
- `--cwd <PATH>` / `-w <PATH>`: Override working directory. Defaults to the client's current directory.
- `<COMMAND> [ARGS...]`: The command and arguments to execute. Required.

### Behavior

1. Parse CLI arguments.
2. Resolve `--env` overrides (expand bare KEYs from host environment).
3. Determine `cwd` (explicit `--cwd` or client's current directory).
4. Connect to the Unix socket at `--socket`.
5. Send a `Request` frame with `command`, `args`, `cwd`, and `env`.
6. Spawn a thread to read stdin into a bounded channel (capacity 32, 8 KiB buffer).
7. Spawn a task to forward stdin reads as `Stdin` frames.
8. Read frames from the server:
   - `Stdout` → write to stdout
   - `Stderr` → write to stderr
   - `Exit` → record exit code, close connection
   - `Error` → report error, exit with code 1
   - `ServerStopping` → print reason, exit with code 143
9. Return the child's exit code.

### Exit Code Propagation

The client exits with the same exit code as the child process, truncated to the range 0–255 (u8). If the server reports an error or the connection fails, the client exits with code 1. If the server is shutting down or the connection drops without an `Exit` frame, the client exits with code 143 (128 + SIGTERM).

## 6. Server (`ncap-server`)

### CLI Interface

```
ncap-server [OPTIONS]
```

Options:

- `--socket <PATH>` / `-s <PATH>`: Unix socket path to listen on. Reads from `NCAP_SOCKET` env var if not provided.
- `--log-dir <PATH>` / `-l <PATH>`: Directory for log files. Reads from `NCAP_LOG_DIR` env var if not provided. Defaults to the socket file's parent directory.

### Logging

The server writes structured JSON logs to timestamped files in the log directory. Each run creates a file named `ncap-server-<unix_epoch>.log` using `tracing` with `tracing-appender` for non-blocking rolling file output. Log level defaults to `"info"` and can be overridden via the `RUST_LOG` environment variable.

### Socket Lifecycle

1. Remove any stale socket file at the configured path.
2. Listen on the Unix socket path.
3. Accept connections in a loop.
4. Each connection is handled in a separate `tokio::spawn` task — the server supports concurrent client connections.
5. Upon receiving SIGTERM/SIGINT, broadcast `ServerStopping` to all active connections, break the accept loop, and exit.

### Connection Handling

For each accepted connection:

1. Read the first frame. If it is not a `Request` frame, close the connection.
2. Parse the `Request` payload.
3. Start the child process with:
   - The specified `command` and `args`.
   - The specified `cwd`.
   - The specified `env` overrides applied on top of the inherited devshell environment.
4. If spawn fails, send an `Error` frame (with the OS error as `error` and context as `cause`) and close the connection.
5. Bridge I/O:
   - Read `Stdin` frames from the client and write to the child's stdin.
   - Read from the child's stdout and send `Stdout` frames.
   - Read from the child's stderr and send `Stderr` frames.
6. When the child exits, send an `Exit` frame with the exit code (`i32`, matching `std::process::ExitStatus::code()`).
7. If the child process fails to complete (e.g., wait error), send an `Error` frame.

### Concurrency

The server must handle multiple concurrent client connections. Each connection is handled independently in its own task. A broadcast channel coordinates graceful shutdown across all active connections.

### Child Process Semantics

- The child process must inherit the devshell environment (the server runs inside a sourced `nix print-dev-env` environment).
- Environment overrides from the `Request` are applied on top of the inherited environment.
- The server must never replace itself with the child process (no `exec`).
- Each child process runs with piped stdio (not a TTY).

## 7. Container Lifecycle (`ncap-ctl`)

### CLI Interface

```
ncap-ctl <SUBCOMMAND>
```

Subcommands:

| Command | Description |
|---------|-------------|
| `init` | Evaluate the devshell, cache it, and start/restart the container |
| `start` | Start the container from a cached devshell |
| `stop` | Stop the running container |
| `restart` | Stop and restart the container |
| `enter` | Enter an interactive shell inside the devshell container |
| `show-options` | Print the expanded runtime arguments |

All subcommands read configuration from environment variables set by `lib.mkShell`.

### `init`

1. Determine the project root via `git rev-parse --show-toplevel` or fall back to `pwd`.
2. Create the cache directory if needed.
3. Check cache validity: the cache is valid only when `NCAP_CACHE=1` AND the env cache file exists. Without direnv, `NCAP_CACHE` is never `1`, so the cache is always regenerated.
4. If invalid: run `nix print-dev-env --profile <profile> <devshell>`, write stdout to `.ncap-cache/<devshell>/env`, run `nix profile wipe-history --profile <profile>` to clean up, then `restart` the container.
5. If valid: `start` the container.

### `start`

1. Check if the container is already running via `<runtime> inspect`. If running, do nothing.
2. Verify the cached env file exists. If missing, error out.
3. Warn if the socket directory is non-empty.
4. Create the socket directory.
5. Run `<runtime> run -d [options] -- <image> <bash> -c "source <env> && exec <server_bin> --socket <socket>"`.

### `stop`

Run `<runtime> stop <container>`. The `ncap-server` entrypoint (PID 1) receives SIGTERM and triggers graceful shutdown of all active connections.

### `restart`

Run `stop` (failures are non-fatal) followed by `start`.

### `enter`

1. Verify the cached env file exists. If missing, error out.
2. Run `<runtime> exec -it <container> <bash> -c "source <env>; exec <bash>"`.
3. Forward the interactive shell's exit code.

### `show-options`

Print each expanded runtime argument on its own line.

### Container Configuration

Runtime options are passed as a JSON array via `NCAP_RUN_OPTS`:

- Mandatory options: `--replace`, `--name <container>`, bind mounts for `/nix:/nix:ro`, project root, and socket directory, `-w $PROJECT_ROOT`.
- Optional hardening (when `harden=true`): `--cap-drop=all`, `--security-opt=no-new-privileges`.
- Extra options via `extraOptions` parameter and environment variable expansion.

Template variables in options (`$PROJECT_ROOT`, `${VAR}`) are expanded at runtime.

## 8. direnv Integration (`ncap-direnv`)

### CLI Interface

```
ncap-direnv
```

No arguments. Reads from environment and current directory only.

### Behavior

1. Read the `DIRENV_WATCHES` environment variable (set by direnv).
2. Run `direnv show_dump <DIRENV_WATCHES>` to get current file modification times.
3. Load previously stored modification times from `.ncap-cache/direnv-mtimes.json`.
4. Compare current mtimes against stored mtimes. The cache is valid when:
   - Stored state is non-empty.
   - All file paths in the stored state have identical mtimes in the current state.
5. Save the current mtimes to `.ncap-cache/direnv-mtimes.json`.
6. Output a shell script that:
   - Sets `NCAP_CACHE=1` if valid, `NCAP_CACHE=0` if invalid.
   - Defines a `use_flake()` function. When invoked, if `NCAP_CACHE=0`, runs `nix print-dev-env "$@"` and caches the output; then sources the cached environment.

### Output Example

```sh
export NCAP_CACHE=1

use_flake() {
  local cache_dir="/path/to/.ncap-cache"
  mkdir -p "$cache_dir"
  if [[ $NCAP_CACHE -eq 0 ]]; then
    nix print-dev-env "$@" > "$cache_dir/env" 2>/dev/null
  fi
  source "$cache_dir/env"
}
```

### Integration Point

The `ncap-direnv` binary is configured as the default flake app (`apps.default`). In a direnv `.envrc` that calls `use flake`, direnv invokes `ncap-direnv` and evals its output. This sets `NCAP_CACHE` before `ncap-ctl init` runs (via `shellHook`), enabling the cache validity check.

## 9. Nix Library (`lib.mkShell`)

### Function Signature

```
mkShell {
  image          # Container image (required)
  devShell       # Nix devShell attribute path (e.g., "container" or ".#container")
  socketPath     # Unix socket path
  containerName  # Container name
  runtime        # Container runtime (default: "podman")
  extraOptions   # Additional runtime options (default: [])
  harden         # Enable hardening flags (default: false)
  autoStart      # Run ncap-ctl init in shellHook (default: true)
  wrappers       # Commands to wrap with ncap (default: [])
  preShellHook   # Shell code before auto-start (default: "")
  postShellHook  # Shell code after auto-start (default: "")
}
```

### devShell Resolution

- If `devShell` starts with `.` or `/`, use as-is.
- Otherwise, prefix with `.#` (e.g., `"container"` → `".#container"`).

### Wrappers

Each entry in the `wrappers` list creates a `writeShellScriptBin` entry in the devShell's `packages`. The wrapper scripts invoke `ncap <value> "$@"`, enabling transparent host usage of container tools.

Entries may be:
- A string: the name and command are the same (e.g., `"cargo"` → `ncap cargo "$@"`)
- An attrset `{ name, value }`: different wrapper name and command

### Environment Variables Set

| Variable | Value |
|----------|-------|
| `NCAP_SOCKET` | `socketPath` |
| `NCAP_DEVSHELL` | Resolved devShell path |
| `NCAP_CONTAINER` | `containerName` |
| `NCAP_IMAGE` | `image` |
| `NCAP_RUNTIME` | `runtime` |
| `NCAP_RUN_OPTS` | JSON array of runtime run options |
| `NCAP_SERVER` | Path to `ncap-server` binary |
| `NCAP_NIX` | Path to `nix` binary |
| `NCAP_BASH` | Path to `bash` binary |

### Runtime Options

The `NCAP_RUN_OPTS` JSON array includes:

- `--replace` and `--name <containerName>`
- Bind mounts: `/nix:/nix:ro`, `$PROJECT_ROOT:$PROJECT_ROOT`, `$socketDir:$socketDir`
- Working directory: `-w $PROJECT_ROOT`
- When `harden=true`: `--cap-drop=all`, `--security-opt=no-new-privileges`
- Any `extraOptions` appended

### shellHook

```
${preShellHook}
${optionalString autoStart "ncap-ctl init"}
${postShellHook}
```

## 10. Execution Semantics

### What `ncap <command> [args...]` Preserves

- **Arguments**: Passed exactly as provided.
- **stdin**: Bridged bidirectionally from the client to the child.
- **stdout**: Bridged bidirectionally from the child to the client.
- **stderr**: Bridged bidirectionally from the child to the client.
- **Exit code**: Propagated from the child to the client.
- **Working directory**: Set to the client's cwd (or `--cwd` override).

### Long-Lived Processes

The system must support long-lived stdio processes such as LSP servers. The connection remains open until the child process exits or the client disconnects.

### Concurrent Execution

Multiple `ncap` invocations may run simultaneously against the same server. Each invocation is independent; I/O streams do not interfere with each other.

### Graceful Shutdown

The server handles SIGTERM/SIGINT directly (as PID 1) and notifies all active clients via `ServerStopping` frames before exiting. Clients receiving the notification exit with code 143. This allows LSP servers and other long-lived processes to terminate cleanly during container shutdown.
