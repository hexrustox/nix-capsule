# Strict flake option type checks

Every `mkShell` option is checked at eval time before `mkShellNoCC` runs — a mismatch throws naming the option, the expected shape, and the received value, with no coercions and no Nix `path` values — so a misconfigured flake fails at `nix develop` evaluation instead of surfacing later as a container launch or runtime failure inside Ctl. The strictness is the point: once lenient coercion ships, downstream flakes depend on it and tightening becomes a breaking change.
