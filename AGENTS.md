# AGENTS.md

## Project Overview

`nix-capsule` is a Rust + Nix Flakes project that provides containerized execution of dev tools via Podman.
The host CLI (`ncap`) sends commands over a Unix socket to a long-lived server (`ncap-server`) running inside a Podman container with a real Nix devshell.

Read `spec.md` for the full project specification before implementing.

## Build / Lint / Test Commands

The Nix devShell is entered automatically via direnv (`use flake . --impure`). All commands below assume you are inside the devShell.

```sh
# Build
cargo build                # debug build
cargo build --release      # release build

# Run
cargo run                  # run the binary
cargo run -- <args>        # run with arguments

# Test
cargo test                 # run all tests
cargo test <test_name>     # run a single test by name

# Lint
cargo clippy               # Rust lints (available in devshell)
cargo clippy -- -D warnings  # deny warnings (use in CI)

# Format
cargo fmt                  # format Rust code
cargo fmt -- --check       # check formatting without writing
nixfmt                     # format Nix files
taplo fmt                  # format TOML files

# Other
cargo deny check           # license/advisory checks
cargo machete              # find unused dependencies
nix flake check            # validate the Nix flake
```

## Project Structure

```
src/           # Rust source code (binary crate)
spec.md        # Project specification — read this first
flake.nix      # Nix flake: devShell and build config
Cargo.toml     # Rust package manifest
.cargo/config.toml  # Linker config (clang + mold)
```

## Code Style

### Rust

- Edition: 2024 (stable toolchain 1.93.1)
- Formatter: `cargo fmt` (default rustfmt settings)
- Linter: `cargo clippy` — fix all warnings
- Run `cargo fmt && cargo clippy -- -D warnings` before finishing a task
- Use `snake_case` for functions, variables, modules; `CamelCase` for types/traits
- Prefer explicit types in public APIs; use `let` with inference for local variables
- Use `anyhow::Result` or `thiserror` for error handling (when added as dependencies)
- Use `clap` for CLI argument parsing (when added)
- Keep `main()` thin — delegate to library modules

### Imports

- Group imports: `std` → external crates → local crate modules
- Use `use` statements rather than full paths in function bodies
- Avoid glob imports (`use foo::*`) in production code

### Modules

- One module per file or per directory with `mod.rs`
- Keep modules small and focused (e.g., `client`, `server`, `protocol`)
- Re-export public API from `lib.rs`

### Nix

- Use `nixfmt` to format all Nix files
- Follow functional style: prefer `let` bindings and `with` sparingly
- Keep `flake.nix` focused on orchestration; put logic in library files if it grows

### TOML

- Use `taplo fmt` to format `Cargo.toml` and other TOML files

### Git

- Commits should be small, focused, and descriptive
- Use conventional-style prefixes when practical (`feat:`, `fix:`, `refactor:`, `chore:`)

## Architecture Notes

Three main components to implement:

1. **`ncap`** — host CLI client; connects to a Unix socket, sends command requests, bridges stdio
2. **`ncap-server`** — long-lived server inside the container; listens on the Unix socket, spawns child processes, bridges their stdio back to the client
3. **Nix library (`lib.mkShell`)** — produces the host-facing devShell with lifecycle scripts (`start-container`, `stop-container`, `restart-container`)

The protocol between client and server is implementation-defined but must preserve: command, args, cwd, env overrides, stdin/stdout/stderr, and exit code.

## Important Constraints

- `ncap-server` must never replace itself with the executed command (no `exec`)
- Multiple concurrent client connections must be supported
- The implementation must not assume a fixed devshell name (e.g., `container`)
- Socket path is configurable, not hardcoded
- Container lifecycle: `finalOpts = defaultOpts - removeOpts + opts`
