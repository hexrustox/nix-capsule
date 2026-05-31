# nix-capsule

## Project Overview

Rust project providing containerized dev tool execution via Unix socket protocol. Five binaries: `ncap` (client), `ncap-server`, `ncap-ctl` (lifecycle management), `ncap-direnv` (direnv integration).

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
- Rust with rust-analyzer
- mold linker (configured in `.cargo/config.toml`)
- cargo-deny, cargo-edit, cargo-machete
- nixd, nixfmt, taplo, codebook

Enter dev shell: `nix develop` or rely on direnv (auto-activates via `.envrc`).

## Architecture

```
src/
  client.rs       ncap       — Host CLI client
  server.rs       ncap-server — Long-lived server inside the container
  ctl.rs          ncap-ctl    — Container lifecycle management
  direnv.rs       ncap-direnv — direnv integration
  protocol.rs                 — Wire protocol framing & types
lib.nix                       — Nix library (mkShell)
```

Communication uses a custom binary framing protocol over Unix sockets.

## Test Patterns

Tests in `tests/` are integration tests that spawn actual server/client processes:
- Use `tempfile::tempdir()` for socket isolation
- Spawn server with `Command::new(NCAP_SERVER)`
- Use `sleep()` for process startup synchronization
- Test binary paths via `env!("CARGO_BIN_EXE_*")` constants
