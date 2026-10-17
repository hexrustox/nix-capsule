# Tests Rewrite Recipe

Handoff recipe for the spec-first rewrite of the `tests/` integration suite.
Home: `docs/agents/tests-rewrite.md` (this file). Hand its path to the session
that does the work; it stands alone — no map, no ticket, no other document.

Scope: the six `tests/*.rs` files — `codec.rs`, `exec.rs`, `signals.rs`,
`disconnect.rs`, `lifecycle.rs`, `ctl.rs`. What the tests pin and when they
fire comes from `spec/`, never from this doc; this doc governs only what a
session loads, in what order, and what "done" means per file. Product code,
`src/` unit tests, and new dev-dependencies are not part of the rewrite. The
human holds the per-file approval gate, not the style choices.

## Load set

A rewriting session loads exactly:

- **`spec/*.md`** — the sole source of truth: `protocol.md`, `client.md`,
  `server.md`, `ctl.md`, `runtime.md`, `env.md`, `paths.md`, `flake-api.md`.
  Wire literals — tags, framing, exit codes — re-derive from `spec/protocol.md`.
- **`CONTEXT.md`** — vocabulary: Client, Server, Ctl, Child, Terminal frame,
  Wrapper, Runtime adapter.
- **skills `writing-tests`, then `rust-writing-tests`** — the whole
  convention set, in that order (the Rust skill is a patch over the base).
  There is no project conventions doc; decide any remaining style on the fly,
  per file.

## Blackout

No rewriting session ever loads any file under `tests/` — neither the file
under rewrite nor `tests/common/`. Old test code never enters the writing
context in any pass. The old suite's pinned behaviors already live in `spec/`
(the absorption pass is complete); a behavior missing from `spec/` is an
unknown (see Escalation), never a reason to peek.

## Conventions

The two skills govern everything about how a test reads and are not restated
here. Standing preferences for this effort:

- **DAMP over DRY.** Plain, explicit, slightly repetitive beats clever and
  shared.
- **Self-contained fixtures.** Each rewritten file defines the helpers it
  needs inline — no `mod common;`, no shared harness. `tests/common/` stays
  untouched and serves only the not-yet-rewritten files.
- **No new dev-dependencies** by default.
- **One behavior, one test**, placed at the narrowest integration point that
  observes it. Walk the `spec/` sections the file's integration point
  reaches; each stated behavior pins a test — the spec is the coverage list.

## Order

Rewrite one file at a time in this order; each merges before the next starts:

| # | file | primary spec anchors |
| --- | --- | --- |
| 1 | `codec.rs` | `protocol.md` |
| 2 | `exec.rs` | `client.md`, `protocol.md` |
| 3 | `signals.rs` | `client.md`, `server.md`, `protocol.md` |
| 4 | `disconnect.rs` | `server.md`, `client.md` |
| 5 | `lifecycle.rs` | `server.md`, `paths.md` |
| 6 | `ctl.rs` | `ctl.md`, `runtime.md`, `env.md`, `paths.md` |

`codec.rs` goes first — pure wire framing, the cheapest file to shake out the
process on — and `ctl.rs` last, the broadest surface. The anchors focus
reading; the load set stands regardless.

## Per-file mechanics

For the file's turn:

1. Branch `test/rewrite-<stem>` off the merge target.
2. Write `tests/<stem>.rs` from the load set alone, with self-contained
   helpers inline (see Conventions).
3. Walk both skill checklists; fix what they flag.
4. `cargo test` green across the whole suite — the remaining old files still
   compile and still pass.
5. Open one PR that replaces `tests/<stem>.rs` at the same path: no other
   file changes, no `tests/common/` changes, no product-code changes.
6. Human approval, merge, then the next file.

## Done-gate

A file is done when all three hold: the `writing-tests` +
`rust-writing-tests` checklists pass, `cargo test` is green, and the human
approves.

## Escalation

A rewriting session decides nothing this recipe and the skills don't already
answer. A genuine unknown — a behavior `spec/` doesn't state, a wire literal
`spec/protocol.md` doesn't pin, a fixture problem inline helpers can't
honestly solve — stops the file. Open a new ticket on the map
[Rewrite recipe for the integration test
suite](https://github.com/hexrustox/nix-capsule/issues/19) and wait for it;
do not improvise.

## After the sixth file

The six PRs leave `tests/common/` with no importers. Deleting or shrinking it
is a separate follow-up, tracked on the map's fog ("`common/` dissolution");
it is not part of any rewrite PR.
