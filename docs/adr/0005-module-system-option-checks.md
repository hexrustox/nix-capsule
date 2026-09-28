# Module-system option checks

`mkShell` enforces its option types with the NixOS module system (`lib.evalModules`) instead of hand-rolled checkers: the option declarations behind the Options table in spec/flake-api.md carry the types and the defaults, so a mismatch fails at eval time with nixpkgs' native wording (`A definition for option … is not of type …`) before `mkShellNoCC` runs (forced by a `deepSeq` over the config). This supersedes ADR 0003's mechanism, keeping its strictness decision: no silent leniency, since downstream flakes come to depend on it and tightening becomes a breaking change.

## Considered options

- **Keep the hand-rolled checkers** (custom type errors naming option, expected shape, received value): they re-implement nixpkgs' type machinery (list-of-strings, bool/int, submodule shapes) and drift from its error conventions for no gain.
- **Leave unknown options to `_module.check`** (drop the pre-filter): the module system's verdict for an unknown top-level option is `The option \`y' does not exist` — accurate, but it abandons the established ``option `x`: unknown option`` wording and its precedence over type errors. The cheap pre-filter over the consumer's literal `args` keeps the verdict and the precedence identical to pre-migration behavior, so flake errors stay stable across the migration. Unknown wrapper *fields* inside `wrappers` are a different matter: the module system's `does not exist` error names the field, which is exactly the required behavior — no pre-filter there.
- **`freeformType` to admit extra attributes**: rejected — it re-admits unknown options, which the strict checks exist to reject.

The `packages` and `override` pass-throughs are declared `raw` (`listOf raw`, `attrsOf raw`): entries stay untyped and unchecked, while the list/attrset container shape itself is still enforced. The `wrappers` string shorthand is declared with `coercedTo`, so the one documented coercion survives the migration; types reject every other wrong shape, including Nix `path` values in string options. Evaluation surfaces the first mismatch and stops, the same one-error-at-a-time behavior the sequential checkers had.
