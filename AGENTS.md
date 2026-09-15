## Agent skills

### Issue tracker

Issues and specs are tracked in this repo's GitHub Issues via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` at the repo root plus `docs/adr/`. See `docs/agents/domain.md`.

### Error messages

Read `docs/agents/error.md` before writing a new or modifying an existing error message.

## Environment (self-hosted)

- This repo's devshell is built by the **pinned** `nix-capsule` flake input
  (`flake.nix` → `gitlab:codnixus/nix-capsule?ref=v0.8.0`), not by the working tree.
  Never repoint that input at a branch or `main` — stable release tags only, and only bump it when explicitly asked.
- Editing `lib.nix`, `spec/`, or `src/` does not change the running devshell or wrappers.
  Verify flake-lib changes in a scratch consumer flake (`inputs.nix-capsule.url = "path:..."`).
- Don't "fix" `.envrc`'s `eval "$(nix run .#)"`: it targets the pinned release's default app
  (`ncap-direnv`); HEAD's app is `ncap-ctl`, which exits with a clap error after building.

## Shell & toolchain

- direnv (`use flake .`) or `nix develop` → Host shell. `cargo`, `rust-analyzer`,
  `nixd`, `taplo`, `codebook-lsp` are Wrappers executing inside the container (podman or docker);
  they fail without a live container (`ncap-ctl status` / `init`).
- Bare host `cargo` fails regardless: linker is hard-fused to clang + mold (`.cargo/config.toml`).
- The Container shell is `.#devShells.container`; enter it only via `ncap-ctl enter`,
  never `nix develop .#container`.

## Build & test

- `cargo build` / `cargo test` (from the Host shell). Single test: `cargo test <name>`;
  single file: `cargo test --test <file>` (codec, ctl, disconnect, exec, lifecycle, signals).
- Integration tests are self-contained: real binaries via `CARGO_BIN_EXE_*` on tempdir sockets,
  stub podman/docker + stub `nix` — no container or Nix needed to run them
  (`tests/common/mod.rs`, `tests/ctl.rs`).
- `package.nix` sets `doCheck = false`: `nix build` never runs tests.
- Lint/format tooling in the container shell: `cargo machete`, `cargo deny`, rustfmt, nixfmt, taplo.
  No clippy. `codebook.toml` is the word list for the codebook-lsp spell checker.
- No CI runs tests or lints (`.github/workflows/` is release-only) — run checks locally.

## Source-of-truth docs

- `spec/` defines behavior … Keep code and specs in sync; change both or neither.
  Divergence checks apply only to the working tree: `lib.nix` vs `spec/flake-api.md`.
  The root `flake.nix` is a *consumer* pinned to the v0.8.0 release — it follows
  that release's API, not the spec. Never flag the root flake against
  `spec/flake-api.md` (see Environment § self-hosted).
- Use `CONTEXT.md` vocabulary exactly: **Host shell** vs **Container shell** (never bare "devshell"),
  **Container** (not capsule/sandbox), **Server** (not daemon), **Wrapper** (not shim).
- Check `docs/adr/` for the area you touch; flag contradictions instead of silently overriding.

## legacy/

- `legacy/` is the pre-rewrite implementation, frozen and excluded from search (`.ignore`).
  Read it for "how things were done" reference only — never import, modify, or build it.
