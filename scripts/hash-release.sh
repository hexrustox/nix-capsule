#!/usr/bin/env bash
set -euo pipefail

system="x86_64-linux"

store_path=$(nix build ".#packages.${system}.default" --no-link --print-out-paths)

tarball="releases/${system}.tar.gz"
mkdir -p releases
tar czf "$tarball" -C "$store_path" bin share

nix hash file --sri --type sha256 "$tarball"
rm "$tarball"
rmdir releases
