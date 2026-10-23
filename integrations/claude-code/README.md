# secondwind for Claude Code

A Claude Code mod that compresses large Bash output through the `secondwind` binary after the
command has run, so quoting, exit codes and permission prompts are untouched. The model reads a
stub with a `<<swload:...>>` marker; the full output stays one `resolve` call away, whole or by
slice. A pane shows what it saved, net of every block the model fetched back.

## Install

The `secondwind` binary must be on `PATH` (see the repository README). Then, at the prompt of a
terminal session:

```
/plugin install secondwind --marketplace orchetron/secondwind
```

Answer `y` to add the marketplace, then pick the user scope. From a shell the same install is
`claude plugin install secondwind --marketplace orchetron/secondwind`. In the desktop app, click
`+` next to the prompt box, then Plugins, add the marketplace `orchetron/secondwind`, then add
the plugin.

## Use

Compression is off in every session until you turn it on there:

- `/secondwind on` or `off` switches it for the current session only; `/secondwind status`
  prints the figures; `/secondwind reset` clears them.
- The pane's `turn on` / `turn off` button does the same (hotkey `p` while the pane has focus).
- The status line under the chat shows `secondwind off`, `secondwind on`, or the net percent.

What the pane shows:

- **touched**: net saved as a share of the tokens secondwind compressed.
- **all output**: net saved as a share of every tool result the model read this session.
- **verified**: compressed blocks the binary verified lossless.
- **raw / sent / back**: tokens in, tokens sent instead, tokens resolves handed back.
- **recent**: the last markers, with `×n` when one was reopened.

Net is raw minus sent minus handed back, so a block the model resolves in full shows as a cost,
in red. `≈` marks a byte estimate; exact figures come from the binary's ledger.

Only Bash results of 6 KB or more are compressed. Claude Code caps a Bash result at 30,000
characters before any plugin sees it, so the mod compresses what remains.

## Develop

```sh
claude plugin validate integrations/claude-code
claude plugin test integrations/claude-code
tsc -p integrations/claude-code      # once Claude Code has laid .claude-plugin/types
```
