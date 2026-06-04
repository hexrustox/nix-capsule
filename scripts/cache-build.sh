#!/usr/bin/env bash
set -euo pipefail

out=$(nix build .#ncap --no-link --print-out-paths)
mkdir -p releases
tar czf releases/x86_64-linux.tar.gz -C "${out}" bin share
