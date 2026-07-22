#!/usr/bin/env sh
# Build (release) and launch Base Search on macOS or Linux.
#   ./run.sh        -> build and run the desktop app
#   ./run.sh cli ... -> build and run the command-line tool with arguments
#
# Requires the Rust toolchain (https://rustup.rs). On Linux you also need the
# GUI build dependencies listed in the README.
set -e
cd "$(dirname "$0")"

if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then
        . "$HOME/.cargo/env"
    fi
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "Rust is not installed. Install it from https://rustup.rs and re-run." >&2
    exit 1
fi

if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    echo "Node.js and npm are required. Install them from https://nodejs.org and re-run." >&2
    exit 1
fi

npm --prefix web-ui ci
npm --prefix web-ui run build
test -f web-ui/dist/index.html
cargo build --release

if [ "$1" = "cli" ]; then
    shift
    exec ./target/release/base-search-cli "$@"
else
    exec ./target/release/BaseSearch
fi
