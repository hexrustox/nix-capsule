# direnv Integration (`ncap-direnv`)

See [`main.md`](main.md) for the cache strategy.

## CLI

```
ncap-direnv
```

No arguments. Reads `DIRENV_WATCHES` from the environment.

## Behavior

1. Read `DIRENV_WATCHES` (set by direnv). If empty, skip (cache invalid).
2. Run `direnv show_dump <DIRENV_WATCHES>` to get current file mtimes.
3. Load stored mtimes from `.ncap-cache/direnv-mtimes.json`.
4. Cache is valid when:
   - Stored state is non-empty.
   - Every current watched path has an identical mtime in the stored state.

   In other words, the cache is invalid if a previously unwatched Nix input now appears with a different mtime. Extra stored entries for files that are no longer watched are ignored.
5. Save current mtimes to `.ncap-cache/direnv-mtimes.json`.
6. Print a shell script to stdout.

## Output

The script is generated from [`src/use_flake.sh`](../src/use_flake.sh) with three template substitutions:

| Placeholder | Replacement |
|---|---|
| `__NCAP_CACHE__` | `1` if cache valid, `0` otherwise |
| `__CACHE_DIR__` | `.ncap-cache` directory path |
| `__ENV_FILE__` | Name of the cached devshell activation script (`env`) |

```sh
export NCAP_CACHE=1

use_flake() {
  local cache_dir="/path/to/.ncap-cache"
  mkdir -p "$cache_dir"
  if [[ $NCAP_CACHE -eq 0 ]]; then
    nix print-dev-env "$@" > "$cache_dir/env"
  fi
  source "$cache_dir/env"
}
```

## Integration Point

`ncap-direnv` is the default flake app (`apps.default`). In `.envrc` with `use flake`, direnv invokes it and evals the output. This sets `NCAP_CACHE` before `ncap-ctl init` runs (via `shellHook`), enabling the cache validity check.
