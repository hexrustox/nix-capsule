#!/usr/bin/env bash
set -euo pipefail

# Constants
readonly DEFAULT_NAME="world"
readonly GREETING_FILE="/tmp/greeting.txt"

# ── Function: greet ──────────────────────────────────
# Prints a greeting message.
# @param $1 Name to greet (defaults to DEFAULT_NAME)
greet() {
    local name="${1:-$DEFAULT_NAME}"
    echo "Hello, ${name}!"
}

# ── Function: write_greeting ─────────────────────────
# Writes greeting text to a file.
# @param $1 Name to greet
write_greeting() {
    local name="$1"
    local message
    message=$(greet "$name")
    echo "$message" > "$GREETING_FILE"
}

# ── Function: read_greeting ──────────────────────────
# Reads and outputs the greeting file.
read_greeting() {
    if [[ -f "$GREETING_FILE" ]]; then
        cat "$GREETING_FILE"
    else
        echo "No greeting file found"
    fi
}

# ── Main dispatch ─────────────────────────────────────
main() {
    local cmd="${1:-}"
    shift 2>/dev/null || true
    case "$cmd" in
        greet) greet "${1:-}" ;;
        write) write_greeting "${1:-}" ;;
        read)  read_greeting ;;
        *)     echo "Usage: $0 {greet|write|read} [name]"; exit 1 ;;
    esac
}

main "$@"
