# Repository Guidance

## Layout

- `src/` contains the Rust library and the `ncap`, `ncap-server`, and `ncap-ctl` binaries.
- `tests/` contains Rust integration tests; run them through Cargo because the harness locates the binaries via `CARGO_BIN_EXE_*` and uses Unix sockets.
- `spec/` contains the authoritative v2 design documents in Markdown. Read the relevant spec before changing behavior.
- Nix integration is in the root `flake.nix`, `template.nix`, and `.envrc`; there is no current `nix/` directory.
- `legacy/` is the previous implementation. Work from the current tree and inspect `legacy/` only when the task explicitly concerns it.

## Commands

- Run `cargo` and other project tools in the container-backed devshell, not directly on the host.
- Use `cargo test` for the full suite, or `cargo test --test exec`, `cargo test --test signals`, `cargo test --test disconnect`, and `cargo test --test codec` for focused integration tests.
- For manual binary or scenario testing, run the script through `bash -c "..."` so it executes in the same container as the binaries.
- For mutation testing, mutate the source directly and restore the source after the test.

## Repository Docs

- Read `CONTEXT.md` for the project vocabulary and `docs/agents/domain.md` for domain-document usage.
- Issues and specs are tracked under `.scratch/<feature>/`; see `docs/agents/issue-tracker.md`.
- Use the canonical triage labels `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`; see `docs/agents/triage-labels.md`.
