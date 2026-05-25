# nix-capsule Specification

## 1. Overview

`nix-capsule` provides explicit host commands that execute inside a long-lived Podman container, within a real Nix devshell already entered inside that container.

Example usage:

```sh
ncap cargo build
ncap nil
ncap bash
```

Goals:

- Containerized execution of dev tools
- Correct Nix devshell environment fidelity
- Low per-command overhead
- Host/editor access to tools available only inside the container

## 2. Architecture

### Components

- **`ncap`**: Host CLI client. Connects to a Unix socket, sends execution requests, bridges stdio between the terminal and the server.
- **`ncap-server`**: Long-lived server inside the container. Listens on the Unix socket, spawns requested commands as child processes, bridges their stdio back to the client. Supports graceful shutdown — notifies active clients before exiting.
- **`ncap-init`**: Container entrypoint. Waits for SIGTERM or SIGINT, then sends a `RequestShutdown` frame to the server, triggering graceful shutdown.
- **Nix library (`lib.mkShell`)**: Produces the host-facing devShell with lifecycle scripts (`start-container`, `stop-container`, `restart-container`, `enter-container`) and wrapper commands.

### Runtime Model

1. `start-container` launches a Podman container running `ncap-init` as the entrypoint.
2. Optionally, `nix develop` is executed inside the container to verify the devshell is realizable.
3. `ncap-server` is started in the background inside the container's devshell.
4. `ncap-server` remains alive, listening on a Unix socket.
5. Each `ncap <cmd>` invocation opens a new connection to the socket, sends a request, and bridges stdio until the child exits.
6. When the container stops, `ncap-init` receives the signal, requests shutdown, and the server notifies active clients before exiting.

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

## 3. Wire Protocol

Communication uses a custom binary framing protocol over a Unix socket.

### Frame Structure

Each frame consists of:

- **Type** (1 byte): Identifies the frame kind
- **Length** (4 bytes, big-endian): Payload length in bytes
- **Payload** (variable): Frame-specific data

### Frame Types

| Type | Value | Payload | Direction |
|------|-------|---------|-----------|
| Request | 0x01 | JSON | Client → Server |
| Stdin | 0x02 | Raw bytes | Client → Server |
| Stdout | 0x03 | Raw bytes | Server → Client |
| Stderr | 0x04 | Raw bytes | Server → Client |
| Exit | 0x05 | JSON | Server → Client |
| Error | 0x06 | JSON | Server → Client |
| RequestShutdown | 0x07 | Empty | Init → Server |
| ServerStopping | 0x08 | JSON | Server → Client |

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

1. `ncap-init` receives SIGTERM or SIGINT when the container stops.
2. `ncap-init` connects to the Unix socket and sends a `RequestShutdown` frame.
3. Server broadcasts a `ServerStopping` frame to all active client connections.
4. Server stops accepting new connections and exits after active connections close.
5. Clients receiving `ServerStopping` print reason if any and exit with code 143 (128 + SIGTERM).

## 4. Client (`ncap`) Requirements

### CLI Interface

```
ncap [OPTIONS] <COMMAND> [ARGS...]
```

Options:

- `--socket <PATH>` / `-s <PATH>`: Unix socket path. Reads from `NCAP_SOCKET` env var if not provided.
- `--env <KEY=VALUE>` / `-e <KEY=VALUE>`: Environment variable overrides. May be specified multiple times.
- `--cwd <PATH>` / `-w <PATH>`: Override working directory. Defaults to the client's current directory.
- `<COMMAND> [ARGS...]`: The command and arguments to execute. Required.

### Behavior

1. Parse CLI arguments.
2. Determine `cwd` (explicit `--cwd` or client's current directory).
3. Connect to the Unix socket at `--socket`.
4. Send a `Request` frame with `command`, `args`, `cwd`, and `env`.
5. Read stdin and send `Stdin` frames.
6. Read frames from the server:
   - `Stdout` → write to stdout
   - `Stderr` → write to stderr
   - `Exit` → record exit code, close connection
   - `Error` → report error, exit with code 1
   - `ServerStopping` → print reason, exit with code 143
7. Return the child's exit code.

### Exit Code Propagation

The client exits with the same code as the child process. If the server reports an error or the connection fails, the client exits with code 1. If the server is shutting down or the connection drops without an `Exit` frame, the client exits with code 143 (128 + SIGTERM).

## 5. Server (`ncap-server`) Requirements

### CLI Interface

```
ncap-server [OPTIONS]
```

Options:

- `--socket <PATH>` / `-s <PATH>`: Unix socket path to listen on. Reads from `NCAP_SOCKET` env var if not provided.
- `--log-dir <PATH>` / `-l <PATH>`: Directory for log files. Reads from `NCAP_LOG_DIR` env var if not provided. Defaults to the socket file's parent directory.

### Socket Lifecycle

1. Remove any stale socket file at the configured path.
2. Listen on the Unix socket path.
3. Accept connections in a loop.
4. Upon receiving a `RequestShutdown` frame from `ncap-init`, broadcast `ServerStopping` to all active connections, stop accepting new connections, and exit.

### Connection Handling

For each accepted connection:

1. Read the first frame. If it is a `RequestShutdown` frame, trigger graceful shutdown. If it is not a `Request` frame, close the connection.
2. Parse the `Request` payload.
3. Start the child process with:
   - The specified `command` and `args`.
   - The specified `cwd`.
   - The specified `env` overrides applied on top of the inherited devshell environment.
   - stdin, stdout, and stderr piped.
4. If spawn fails, send an `Error` frame and close the connection.
5. Bridge I/O:
   - Read `Stdin` frames from the client and write to the child's stdin.
   - Read from the child's stdout and send `Stdout` frames.
   - Read from the child's stderr and send `Stderr` frames.
6. When the child exits, send an `Exit` frame with the exit code.
7. If the child process fails to complete, send an `Error` frame.

### Concurrency

The server must handle multiple concurrent client connections. Each connection is handled independently in its own task.

### Child Process Semantics

- The child process must inherit the devshell environment (the server runs inside `nix develop`).
- Environment overrides from the `Request` are applied on top of the inherited environment.
- The server must never replace itself with the child process (no `exec`).
- Each child process runs with piped stdio (not a TTY).

## 6. Container Lifecycle

### `start-container`

1. Determine `project_root` via `git rev-parse --show-toplevel` or fall back to `pwd`.
2. Create the socket directory if it doesn't exist.
3. Launch the container with `ncap-init` as the entrypoint.
4. Optionally verify the devshell is realizable inside the container (controlled by the `wait` parameter).
5. Start `ncap-server` in the background inside the container's devshell.

### `stop-container`

Stop the container. The container's `ncap-init` entrypoint receives SIGTERM and triggers graceful shutdown of the server and all active connections.

### `restart-container`

Run `stop-container` followed by `start-container`.

### `enter-container`

Open an interactive shell inside the container's devshell.

### `autoStart`

When enabled (default), the shell hook checks if the container is running before each shell session. If not, it automatically runs `start-container`.

## 7. Execution Semantics

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

When the container stops, `ncap-init` signals the server. The server notifies all active clients via `ServerStopping` frames before exiting. Clients receiving the notification exit with code 143. This allows LSP servers and other long-lived processes to terminate cleanly during container shutdown.
