# 01: Cargo skeleton + wire-protocol codec

**What to build:** The repository builds three binaries — `ncap`, `ncap-server`, `ncap-ctl` — from one crate over a shared library module, and the v2 wire protocol exists as code: every frame is 1 tag byte + a 4-byte big-endian payload length; `Request`, `Exit`, `Error`, `Version`, and `Signal` carry JSON payloads, `Stdin`/`Stdout`/`Stderr` carry raw bytes. All nine frame tags from `spec/protocol.md` are modeled. A declared payload length above the 16 MiB cap is a transport violation surfaced by the decoder. The v1 implementation stays untouched under `legacy/` as reference only — nothing is migrated wholesale (v2 drops the direnv binary entirely).

This is the shared seam everything else tests against; no behavior beyond the codec is in scope.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] All three binaries build and respond to `--help`
- [ ] Codec round-trips every frame type at the byte level (unit tests)
- [ ] Stream frames round-trip arbitrary binary payloads chunk-agnostically
- [ ] A frame declaring more than 16 MiB is rejected as a transport violation by the decoder
- [ ] Truncated/malformed input fails decoding without panicking
