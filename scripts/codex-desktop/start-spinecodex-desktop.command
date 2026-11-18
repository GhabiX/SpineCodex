#!/bin/bash
set -euo pipefail

show_error() {
    local message="$1"
    local exit_code="${2:-1}"
    trap - ERR

    if command -v osascript >/dev/null 2>&1; then
        if ! osascript \
            -e 'on run argv' \
            -e 'display dialog (item 1 of argv) with title "SpineCodex Desktop Launcher" buttons {"OK"} default button "OK" with icon stop' \
            -e 'end run' \
            "$message" >/dev/null 2>&1
        then
            printf '%s\n' "$message" >&2
        fi
    else
        printf '%s\n' "$message" >&2
    fi

    exit "$exit_code"
}

handle_error() {
    local exit_code=$?
    local failed_command="${BASH_COMMAND:-unknown command}"
    show_error $'SpineCodex Desktop launch failed.\n\n'"$failed_command" "$exit_code"
}

codex_desktop_is_running() {
    [[ "$(osascript -e 'application id "com.openai.codex" is running' 2>/dev/null || true)" == "true" ]]
}

trap handle_error ERR

if [[ "$(uname -s)" != "Darwin" ]]; then
    show_error "This script must be run on macOS."
fi

if ! command -v npm >/dev/null 2>&1; then
    show_error "npm was not found in PATH."
fi

if ! command -v node >/dev/null 2>&1; then
    show_error "Node.js was not found in PATH."
fi

npm_root="$(npm root -g 2>/dev/null || true)"
if [[ -z "$npm_root" ]]; then
    show_error "Could not determine the global npm modules directory."
fi

node_arch="$(node -p 'process.arch' 2>/dev/null || true)"
case "$node_arch" in
    arm64)
        codex_path="$npm_root/@spinejit/spine-codex/node_modules/@spinejit/spine-codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex"
        ;;
    x64)
        codex_path="$npm_root/@spinejit/spine-codex/node_modules/@spinejit/spine-codex-darwin-x64/vendor/x86_64-apple-darwin/bin/codex"
        ;;
    *)
        show_error "Unsupported Node.js architecture: $node_arch"
        ;;
esac

if [[ ! -x "$codex_path" ]]; then
    show_error $'Could not find the native @spinejit/spine-codex binary:\n\n'"$codex_path"
fi

if codex_desktop_is_running; then
    show_error 'Codex Desktop is already running. Quit it before retrying.' 2
fi

if ! open --env "CODEX_CLI_PATH=$codex_path" --env "CODEX_SPINE_APP_UI=1" -b "com.openai.codex"; then
    show_error "Could not start Codex Desktop."
fi

echo "Codex Desktop started with native npm SpineCodex: $codex_path"
