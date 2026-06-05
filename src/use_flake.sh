export NCAP_CACHE=__NCAP_CACHE__

use_flake() {
  local cache_dir="__CACHE_DIR__"
  mkdir -p "$cache_dir"
  if [[ $NCAP_CACHE -eq 0 ]]; then
    nix print-dev-env "$@" > "$cache_dir/env"
  fi
  source "$cache_dir/env"
}
