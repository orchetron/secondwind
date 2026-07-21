#!/bin/sh
# Fresh, isolated ledger so the demo's `watch` pane starts at zero and climbs live.
rm -rf /tmp/sw_demo && mkdir -p /tmp/sw_demo
# Release binary carries the logo + watch wordmark; build if missing.
[ -x target/release/secondwind ] || cargo build --release -p secondwind >/dev/null 2>&1
# Clear any prior demo session on our dedicated socket (never touches your real tmux).
tmux -L swdemo kill-server 2>/dev/null || true
