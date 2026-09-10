# Flat TMPDIR socket fallback without uid level

The per-project socket falls back to `$TMPDIR/nix-capsule/<project>/` (with `$TMPDIR` defaulting to `/tmp`) when `XDG_RUNTIME_DIR` is unset, with no uid level in the path — accepting the theoretical collision of two users sharing one `$TMPDIR` and one project name in exchange for path simplicity and parity with the `XDG_RUNTIME_DIR` shape. The isolation that matters (mode `0700` socket dir, typically per-user tmpfs) is enforced by the directory itself, not the path.
