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
Type "tmux -L swdemo -f examples/demo/tmux.conf new-session -s sw 'bash examples/demo/offline.sh; sleep 8' \; split-window -h -l 42% 'target/release/secondwind --home /tmp/sw_demo watch' \; select-pane -t 0"
Enter
Show
Sleep 24s
