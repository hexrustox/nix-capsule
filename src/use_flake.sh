# NCAP_CACHE is a transient signal set by ncap-direnv: 1 means the direnv
# watched inputs are unchanged, so the existing cache can be sourced as-is.
# It is not a user-configurable path or toggle.
export NCAP_CACHE=__NCAP_CACHE__

use_flake() {
  local cache_dir="__CACHE_DIR__"
  mkdir -p "$cache_dir"
  if [[ $NCAP_CACHE -eq 0 ]]; then
    nix print-dev-env "$@" > "$cache_dir/__ENV_FILE__"
  fi
  source "$cache_dir/__ENV_FILE__"
}
