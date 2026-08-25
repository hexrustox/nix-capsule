# 09: Flake surface: overlay + `mkShell`

**What to build:** A consuming flake declares two devshells and gets the whole workflow. An overlay ships the `ncap` package (all three binaries); a lib function exposes `mkShell`, wrapping `mkShellNoCC`, implementing every option from `spec/nix.md`: `project`, `image`, `devShell` (bare names get `.#` prefixed, full URIs pass through), `watchFiles` (defaults to flake.nix + flake.lock; entries that are absolute or contain `..` rejected at evaluation time; plain strings only), `envForward`, `wrappers` (string shorthand and attrset form `{ name, command, env, cwd }` mirroring client flags), `extraOptions`, `harden`, `timeout`, `socketPath`/`containerName` overrides, `preShellHook`/`postShellHook`, `autoStart`.

The produced host shell puts `ncap` and `ncap-ctl` on PATH, writes one PATH shim per wrapper (`exec ncap …` with pre-filled env/cwd flags), exports the complete `NCAP_*` contract including the store paths the ctl needs, and runs a shellHook in the specified order: pre-hook → export `NCAP_PROJECT_ROOT` (git toplevel, falling back to pwd) → guarded `watch_file` emission per watchFiles entry (inert outside direnv, additive with it, no opt-out knob) → `init` when autoStart → post-hook. A failing `init` prints a warning on stderr but never aborts shell entry — wrapped commands surface it later via the client's connect-error hint. Derived defaults follow the spec's name/path tables.

The repo's own flake/template consume this v2 surface, kept honest by the eval-level checks below (runnable wherever Nix exists).

**Blocked by:** 03 (Client env layering), 07 (Real container bring-up).

**Status:** ready-for-agent

- [ ] Instantiating a sample host shell exposes `NCAP_*` values consistent with the option table, including store paths and JSON-array vars
- [ ] Derived defaults correct: sanitized project name; socket/container/cache/log paths keyed by it; XDG fallback rule represented
- [ ] `devShell` URI normalization: bare name prefixed, full URI untouched
- [ ] Absolute or `..` watchFiles entries fail at mkShell evaluation with a clear message
- [ ] Wrapper shims: string form and attrset form produce scripts matching their client-flag mapping
- [ ] ShellHook fragments appear in the required order; `watch_file` lines carry the direnv guard; autoStart off omits the init call
- [ ] Repo's own flake/template instantiate cleanly against the new surface
