#!/bin/bash
# Stream a single window - click to select which one
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WINDOW_PICK="$SCRIPT_DIR/target/release/window-pick"
FOUNDRY="$SCRIPT_DIR/target/release/foundry"

# Check binaries exist
if [[ ! -x "$WINDOW_PICK" ]] || [[ ! -x "$FOUNDRY" ]]; then
    echo "Building release binaries..."
    cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"
fi

# Pick window and stream
WINDOW_ID=$("$WINDOW_PICK" --format=id)
echo "Streaming window $WINDOW_ID"
exec "$FOUNDRY" --window "$WINDOW_ID"
