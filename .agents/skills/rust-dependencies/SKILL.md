---
name: rust-dependencies
description: "Manage a Rust project's Cargo.toml dependencies — add or install a dependency or dev-dependency, bump a version requirement, or wire a workspace member to a shared dependency. Use when adding, installing, or bumping a `[dependencies]` or `[dev-dependencies]` entry, editing Cargo.toml, or wiring a crate to a workspace-shared dependency."
---

# Rust dependency management

Add a dependency the project can actually build on: the version requirement always names the exact `major.minor.patch` minimum, and in a workspace every version lives in one shared table. Two rules drive every edit: **state the minimum** precisely, **route** it to the right table.

## The version requirement

Write every version requirement as the full three components, no operator: `serde = "1.2.3"`. This is Cargo's default requirement and it sets the *minimum* allowed version (`>=1.2.3, <2.0.0`) — it marks the floor your code needs, not a lock, so `cargo update` still moves within the range. It is the documented form. Never write `"1"`, `"1.2"`, `"^1.2"`, `"^1.2.3"`, `"=1.2.3"`, `"~1.2.3"`, or a wildcard, in any table. Applies to every requirement you write or bump — the version in `[dependencies]`, `[dev-dependencies]`, and `[workspace.dependencies]` alike.

## Finding the version

Run `cargo add <dep> --dry-run` and read the exact version from its output. `--dry-run` writes nothing. From a workspace root, add `-p <pkg>` when the target is a member. Never guess a version. If the dep is already declared in the workspace or appears in `Cargo.lock`, reuse that version instead of querying. Do not let `cargo add` make the edit: it cannot place the version where this skill puts it, so take the version from its output and hand-write the lines below.

## Workspace or standalone?

Three setups, one routing rule each:

1. **Standalone crate** — the manifest has no `[workspace]` table and none of its ancestors does. Put the requirement directly in the file's `[dependencies]` or `[dev-dependencies]`.
2. **Workspace root** — the manifest being edited has a `[workspace]` table. Version definitions go in this same file's `[workspace.dependencies]`; the root package activates each one it uses via `workspace = true` (below).
3. **Workspace member** — the crate sits under a root's `members`/`exclude` globs, or its `[package]` has `workspace = "<path>"`. Find the workspace root, put the version in *that* file's `[workspace.dependencies]`, and activate in the member's `[dependencies]` / `[dev-dependencies]`.

The workspace root is the `Cargo.toml` whose `[workspace]` lists the member — a virtual manifest (no `[package]`) works the same way.

## Workspace routing

`[workspace.dependencies]` is the only shared table — it holds the version for every dependency, dev-dependency, and build-dependency used by any member. There is no separate dev-dependencies table; a member inherits a normal or dev dependency the same way.

- The version (stated at its `major.minor.patch` minimum) goes in the root's `[workspace.dependencies]`, never in a member's file.
- The using crate activates it: `serde.workspace = true` under the matching section (`[dependencies]` for a normal dep, `[dev-dependencies]` for a dev-dep).
- The activation line may carry `features` and `optional` only — `serde = { workspace = true, features = ["derive"] }`. No `version`, no `default-features`, no other key on the activation line.
- `[workspace.dependencies]` itself may not declare `optional`.
- If the dep is already declared in `[workspace.dependencies]`, do not add a copy or a second version — raise the existing requirement to the full `major.minor.patch` if it is not, then just add the activation line.

## Features and defaults

The activation line's `features` are there to be used — enable only what the crate actually calls. When a dependency's default features pull heavy or risky code (a large dependency tree, a rendering backend, a `full` bundle), set `default-features = false` and name the features the code needs. Weigh this per dependency, not as a blanket rule: minimal, stable defaults are worth leaving alone; a self-maintained feature list drifts when upstream changes its defaults; and feature unification means any other crate in the graph — another workspace member, a transitive dep — that enables a shared dependency's defaults re-enables them for you too, so `default-features = false` is a floor you can raise, never a guarantee.

## Worked example

A member `crates/app` needs `serde` as a dependency and `tempfile` as a dev-dependency:

`Cargo.toml` (workspace root):

```toml
[workspace.dependencies]
serde = "1.0.219"
tempfile = "3.20.0"
```

`crates/app/Cargo.toml`:

```toml
[dependencies]
serde = { workspace = true, features = ["derive"] }

[dev-dependencies]
tempfile.workspace = true
```

## Completion

Done when: every requirement you wrote or bumped carries the exact `"major.minor.patch"` minimum; in a workspace, the versions sit only in the root's `[workspace.dependencies]`; every crate that uses a shared dep has a `workspace = true` activation; and nothing else in the manifest changed.