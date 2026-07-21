# secondwind demo — a live coding-agent session, with the `watch` pane showing tokens saved climb.
#
# Run from the repo root:   vhs examples/demo.tape
# Produces:                 examples/demo.gif  +  examples/demo.mp4
#
# Requires: tmux, vhs, ffmpeg; a built `target/release/secondwind`; Claude Code logged in.
# The left pane is a real `secondwind run -- claude` session; the right pane is `watch`, reading
# the same demo-scoped ledger so it starts at zero and climbs live as Claude's tool output compresses.
#
# TIMING: Claude's responses vary in length. If a response is cut off, raise the Sleep after that
# query; if there is dead air, lower it. (You can also swap a Sleep for `Wait+Screen /your-regex/`.)

Output "examples/demo.gif"
Output "examples/demo.mp4"

Set Shell "bash"
Set Theme "Hardcore"
Set Width 1840
Set Height 860
Set FontSize 14
Set Framerate 24
Set Padding 26

Hide
Type "bash examples/demo/setup.sh"
Enter
Sleep 2s
# Split: left = your agent through secondwind, right = the live watch pane (same demo ledger).
Type "tmux -L swdemo -f examples/demo/tmux.conf new-session -s sw 'target/release/secondwind --home /tmp/sw_demo run -- claude --dangerously-skip-permissions' \; split-window -h -l 42% 'target/release/secondwind --home /tmp/sw_demo watch' \; select-pane -t 0"
Enter
Sleep 12s
Show

# --- query 1: read a couple of source files (big tool outputs -> watch climbs) ---
Type "read crates/optimize/src/columnar.rs and text_columnar.rs, then briefly name the codecs they define"
Sleep 1s
Enter
Sleep 40s

# --- query 2: a huge command output (cargo metadata JSON) -> a big offload ---
Type "run cargo metadata --format-version 1 and tell me how many workspace members there are"
Sleep 1s
Enter
Sleep 34s

# --- exit the agent -> secondwind prints the session receipt in the left pane ---
Ctrl+C
Sleep 1s
Ctrl+C
Sleep 6s
