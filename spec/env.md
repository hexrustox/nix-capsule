# Environment layering

A Child inside the container sees four layers; for a given KEY, the highest
layer defining it wins. This file is the single source of truth for the
layering and the merge rules; spec/client.md § Request construction,
spec/server.md § Connection handling, and spec/flake-api.md § Options
reference it and never restate it.

| # | Layer | Resolved | Changes take effect |
| --- | --- | --- | --- |
| 1 | Env dump (Server's inherited env) | at init, inside the container | after re-eval + restart |
| 2 | `envForward` | by the Client, from the host env, **per request** | immediately |
| 3 | wrapper `env` | in the wrapper script | after flake edit (wrapper regen) |
| 4 | `-e KEY=VALUE` | CLI flag, per invocation | immediately |

## Merge rules

The Client merges layers 2–4 into the request's env list, in order: every name
in `NCAP_ENV_FORWARD` (a JSON array of variable names) resolved from the
Client's own environment first, then every `--env` flag — wrapper `env`
entries arrive as `--env` flags (layer 3), CLI `-e` flags as `--env` flags
(layer 4). Later wins by key, deduplicated: setting an already-present key
replaces its value in place, keeping the first occurrence's position. Unset
entries are silently omitted. The Server applies that list over its inherited
environment.

- Forwarded *values* are re-read per invocation — a changed host value reaches
  the next command without any restart. Only editing the `envForward` *list*
  touches the flake — which trips the freshness hash and restarts. Incidental,
  not required.
- `NCAP_ENV_FORWARD` that is not a JSON array of names is a local Client
  error (exit `1`).
- An `--env` entry without `=` is rejected by the Server with terminal
  `Error`; entries override same-named inherited keys and never clear
  inherited keys.

## Flag forms

Valid `--env` forms are `KEY` and `KEY=VALUE`: bare `KEY` copies that variable
from the Client's environment if set, silently omitted otherwise. `KEY=` is an
explicit empty value; a value may itself contain `=`. An empty key (`=VALUE`,
or the empty string) is a usage error. Flags are lossy-decoded before parsing,
so non-UTF-8 bytes arrive as `U+FFFD`, never rejected.
