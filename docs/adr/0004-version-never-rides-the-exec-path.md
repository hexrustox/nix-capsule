# Version never rides the exec path

Client and Server ship from the same package, so any version divergence means a live Container was built from an older nix-capsule — and Freshness hashes Watched files, not the package version, so nothing auto-heals the skew (it survives `init`'s live-and-fresh "done" and `start`'s "live ⇒ done"). We therefore removed the version field from `Request` and the Server's handshake `Version` reply entirely: exec Connections carry no version frames, avoiding per-connection chatter whose only use was a warning nobody could act on. Version skew is instead detected by Ctl on demand via the explicit `RequestVersion`/`ServerVersion` probe Connection, warning (never failing) at `status` and `start` with the advice `ncap-ctl restart` — the one channel that does not touch the Child's stdio and does not disturb command exit codes.

## Considered options

- **Keep the advisory handshake** (warn on mismatch in the Server's log): invisible to the user — the Server's log lives inside the container — and every exec Connection paid latency for it.
- **Warn from the Client per connection**: ruled out — the Client's stdout/stderr are the Child's streams; any extra output corrupts tool output, and the only non-fatal surface would interfere exactly where precision matters.
- **Stamp file in the Cache** recording the version at `init`: rejected — a stamp describes what was *built*, not what is *running*, and goes stale precisely when skew exists.
