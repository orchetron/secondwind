# secondwind demo

A split-screen recording: a coding agent on the left, `secondwind watch` on the right showing
tokens saved climb live as the agent's tool output is compressed, ending on the session receipt.

![secondwind demo](../demo.gif)

The `watch` pane reads a demo-scoped ledger (`--home /tmp/sw_demo`), so it starts at zero and the
numbers you see are real, produced by real compression.

## Record the live version (a real Claude Code session)

```sh
vhs examples/demo.tape          # -> examples/demo.gif + examples/demo.mp4
```

Requirements: [`vhs`](https://github.com/charmbracelet/vhs), `tmux`, `ffmpeg`, a built
`target/release/secondwind` (the tape builds it if missing), and Claude Code logged in.

Claude's responses vary in length, so the tape's `Sleep` values are a starting point: if a
response gets cut off raise the `Sleep` after that query, if there's dead air lower it. The tape
passes `--dangerously-skip-permissions` so the recording runs unattended; drop it if you would
rather approve each tool call.

## Preview it without any API calls

```sh
vhs examples/demo/offline.tape  # -> examples/demo.gif + examples/demo.mp4
```

This runs a stand-in agent whose "tool calls" are real `secondwind exec` compressions, so the
`watch` pane still climbs with genuine numbers. It is deterministic and uses no quota; the
committed `demo.gif` is generated this way. It is a re-enactment of the layout, not a live model
session, which is what `demo.tape` records.

## Files

- `demo.tape` — the live recording script (parent dir).
- `demo/setup.sh` — resets the demo ledger and builds the binary if needed.
- `demo/tmux.conf` — a clean, status-bar-free tmux for the split (isolated on its own socket).
- `demo/offline.sh` / `demo/offline.tape` — the no-quota preview.
