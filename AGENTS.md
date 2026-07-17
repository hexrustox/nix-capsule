# nix-capsule

## Project Overview

Rust project providing containerized dev tool execution via Unix socket protocol. Four binaries: `ncap` (client), `ncap-server`, `ncap-ctl` (lifecycle management), `ncap-direnv` (direnv integration). Nix flake wraps the binaries and generates shell wrappers that route host commands through the container.

## Build & Test Commands

```bash
cargo build
cargo test
cargo test init_sends_shutdown_and_exits       # single test
cargo test --test server                        # single test file
```

Linker is hard-fused to `clang` + `mold` via `.cargo/config.toml` — both must be on PATH (the dev shell provides them).

Two flake devShells exist: `default` wraps `cargo`/`rust-analyzer`/etc. through the container via `ncap`; `container` holds the real toolchain. `package.nix` sets `doCheck = false`, so `nix build .#ncap` does not run tests — run `cargo test` explicitly.

## Architecture

```
src/
  client.rs    ncap         — Host CLI client
  server.rs    ncap-server  — Long-lived server inside the container
  ctl.rs       ncap-ctl     — Container lifecycle (init/start/stop/restart/enter/status/log/clean) + `completions`
  direnv.rs    ncap-direnv  — direnv cache integration
  protocol.rs              — Wire protocol framing & types
  path.rs                  — Cache directory paths
  lib.rs                   — Library crate (exposes `path` + `protocol`; used by tests)
lib.nix                    — Nix library (mkShell, app)
package.nix                — Source build (mold + clangStdenv, generates shell completions)
release.nix                — Pulls prebuilt tarball from GitHub Releases (useCache=true) or delegates to package.nix
```

## Test Patterns

Tests in `tests/` are integration tests that spawn real server/client processes via `CARGO_BIN_EXE_*`:
- `tests/common/mod.rs` defines `TestServer` — spawns `ncap-server` on a tempdir socket, polls for socket existence, exposes `run(&["--", cmd, ...])`.
- Use `tempfile::tempdir()` for socket isolation.
- Test binary paths via `env!("CARGO_BIN_EXE_ncap")` / `env!("CARGO_BIN_EXE_ncap-server")`.

## Lint & Format

Available tooling: `cargo-machete` (unused deps), `cargo-deny` (advisories/licenses), `cargo-edit`. Nix: `nixfmt`; TOML: `taplo`. No `clippy` configured.

## Release

Tag `v*` triggers `.github/workflows/release.yml`: `nix build .#packages.x86_64-linux.ncap`, tar `bin share`, upload to GitHub Releases. `release.nix` fetches that tarball by `Cargo.toml` version — bumping the version also requires updating the `hash` in `release.nix`. The overlay uses the prebuilt binary by default (`useCache=true`); set `useCache=false` to build from `package.nix`.

## Agent skills

### Issue tracker

Issues live as markdown files under `.scratch/<feature>/` in this repo (no remote tracker). See `docs/agents/issue-tracker.md`.

### Triage labels

Five canonical roles, each label string equal to its name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context — `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
