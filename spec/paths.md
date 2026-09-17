# Paths, identity, and freshness

This file is the single source of truth for project identity (name, XDG
layout, stamp guard) and cache freshness (digest, states, cache files). All
other files reference it and never restate it.

## Project name

The project name is the sanitized basename of the project root: every
non-ASCII-alphanumeric character joins the surrounding run into a single `-`;
leading and trailing `-` are stripped; case is preserved. Sanitizing runs
character by character: an alphanumeric run emits as-is, a non-alphanumeric
run collapses to one `-` separator (only between two alphanumeric runs — no
leading `-` is ever emitted). An empty result is a hard error naming the
project root, plus a sibling line advising to set `project`. Explicit
`NCAP_PROJECT`, `NCAP_CONTAINER`, `NCAP_SOCKET`, `NCAP_CACHE_DIR`, and
`NCAP_LOG_DIR` bypass their derived values; an unset `NCAP_CONTAINER` derives
as `ncap-<project>`.

## XDG layout

All per-project state lives outside the project tree, keyed by project name.
An explicit `NCAP_SOCKET`, `NCAP_CACHE_DIR`, or `NCAP_LOG_DIR` wins; else the
derived value below applies:

| What | Path |
| --- | --- |
| Socket dir | `$XDG_RUNTIME_DIR/nix-capsule/<project>/` — created mode `0700` |
| Socket | `<socket dir>/ncap.sock` |
| Cache | `$XDG_CACHE_HOME/nix-capsule/<project>/`, else `$HOME/.cache/nix-capsule/<project>/` |
| Logs | `$XDG_STATE_HOME/nix-capsule/<project>/logs/`, else `$HOME/.local/state/nix-capsule/<project>/logs/` |

- If `XDG_RUNTIME_DIR` is unset, the socket falls back to
  `$TMPDIR/nix-capsule/<project>/` — an empty `TMPDIR` is ignored and
  `$TMPDIR` defaults to `/tmp` — dir `0700`. The `0700` mode applies when the
  socket dir is newly created; an existing dir is left untouched.
- For the cache and log dirs, neither the XDG var nor `HOME` being set is an
  error naming the missing variable — there is no further fallback.
- The socket directory is per-user and mode `0700`, typically on tmpfs — it
  disappears at logout/reboot, so stale sockets don't accumulate.
- The log dir and the cache dir are created with plain directory creation (no
  mode pin); only the socket dir carries the `0700` rule.

## Stamp guard

The first cache touch of `init`/`start` is the stamp guard (note: `restart`
runs a runtime-only, non-fatal `stop` before delegating to `init`, so its
first *cache* touch is still the guard): read `<cache>/project`. Present and
different from the current project root (compared after trimming trailing
`\n`/`\r`) ⇒ hard error naming the project and the keyed root, plus a
sibling line advising to set `project` — two checkouts of one repo must never
share a socket/container/cache. Absent ⇒ write it (creating the cache dir if
needed). The same root passes silently.

## Cache contents

| File | Contents |
| --- | --- |
| `env` | the `nix print-dev-env` dump of the Container shell |
| `hash` | xxhash64 of the watched files, lowercase hex, no trailing newline |
| `profile` | the nix profile `print-dev-env` writes; history pruned after each eval |
| `project` | the stamp file (absolute project-root path) |

`clean` removes exactly these four named files plus the `profile-<N>-link`
generation links (a `profile-` prefix with an all-digit middle and a `-link`
suffix; symlinks removed without following, real directories recursed) and
every `ncap-server-<epoch-millis>.log` file in the log dir (best-effort dir
removal when empty), and deletes the socket file + best-effort parent dir —
never recursive on an explicit path that may be shared.

## Freshness and the digest

The digest: xxhash64 (seed 0) over the concatenation of one record per entry,
entries sorted by relative path — `(relative path, NUL, exists flag, NUL,
contents-or-empty)` where the exists flag is `1`/`0`, and file contents stream
into the hasher without full buffering. Missing files contribute their absence
flag, so a file appearing or disappearing flips freshness; a mtime-only touch
does not; the relative path is part of the record, so moving a watched file
flips it too. The empty watch list digests the empty input (`ef46db3751d8e999`).
Cached as lowercase hex, no trailing newline.

Freshness states: **missing** (no `env` file in the Cache — checked first, so
a present `hash` without an `env` dump is still missing), **fresh** (`env`
present and `hash` equals the computed digest after trimming surrounding
whitespace, so a stray trailing newline still reads as fresh), **stale**
(everything else — hash differs, hash unreadable, or the digest computation
itself fails; a compute error is never fresh, even against an empty cached
hash).

Ensuring the Cache: fresh ⇒ nothing happens — no Nix evaluation. Stale or
missing ⇒ `nix print-dev-env --profile <cache>/profile <devshell>` runs on
the host, its stdout written to `<cache>/env`, the profile's history wiped
after each eval (a failed prune is non-fatal), and the new digest computed
and stored.

The trigger surface — shell entry, direnv reload, the no-auto-refresh
consequences — is spec/flake-api.md § Freshness triggers.

## Server log files

The Server logs to `<log-dir>/ncap-server-<epoch-millis>.log` (millisecond
epochs keep runs started in the same second apart). A filename counts as a
server log only when it is exactly `ncap-server-<digits>.log`. Newest =
highest epoch stamp, compared numerically; anything else in the dir is
ignored. No log file ⇒ error naming the log dir.
