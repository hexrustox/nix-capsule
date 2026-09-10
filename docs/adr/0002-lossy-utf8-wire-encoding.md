# Lossy UTF-8 wire encoding

`command`, `args`, `env`, and `cwd` travel as UTF-8 JSON strings with non-UTF-8 host bytes converted lossily (`U+FFFD`) rather than rejecting the request or silently omitting variables — a command that can be named should be deliverable even when the host bytes are not clean Unicode, with the mojibake risk accepted and documented. Strict rejection stays available at the layer above (spawn failures still surface as `127`/`126`/`Error`).
