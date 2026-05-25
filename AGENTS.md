# nix-capsule

## Project Overview

Rust project providing containerized dev tool execution via Unix socket protocol. Three binaries: `ncap` (client), `ncap-server`, `ncap-init`.

## Build & Test Commands

```bash
# Standard Rust commands (requires mold linker on Linux)
cargo build
cargo test

# Run a single test
cargo test init_sends_shutdown_and_exits
cargo test stdout_bridging

# Run tests in a specific file
cargo test --test server
cargo test --test init
```

## Development Environment

This project uses Nix flakes with direnv. The dev shell provides:
- Rust 1.93.1 (stable) with rust-analyzer
- mold linker (configured in `.cargo/config.toml`)
- cargo-deny, cargo-edit, cargo-machete
- nixd, nixfmt, taplo, codebook

Enter dev shell: `nix develop` or rely on direnv (auto-activates via `.envrc`).

## Architecture

- **Protocol**: Custom binary framing over Unix sockets (1 byte type + 4 byte length + payload)
- **Client** (`src/client.rs`): Connects to socket, sends Request, bridges stdio
- **Server** (`src/server.rs`): Long-lived process in container, handles concurrent connections
- **Init** (`src/init.rs`): Container entrypoint, sends RequestShutdown on SIGTERM/SIGINT
- **Nix library** (`lib.nix`): `mkShell` produces host-facing devShell with lifecycle scripts

## Test Patterns

Tests in `tests/` are integration tests that spawn actual server/client processes:
- Use `tempfile::tempdir()` for socket isolation
- Spawn server with `Command::new(NCAP_SERVER)`
- Use `sleep()` for process startup synchronization
- Test binary paths via `env!("CARGO_BIN_EXE_*")` constants
