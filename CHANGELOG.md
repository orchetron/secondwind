# Changelog

## v0.5.0

### Upgrade notes

- **The Bash hook is retired.** Claude Code checks permission rules against the command a
  PreToolUse hook returns, and the hook rewrote every Bash command to `secondwind exec -c '...'`
  while answering "allow", so Bash deny rules stopped matching and prompts were skipped. `secondwind
  hook` now passes every command through unchanged, `setup --hook` installs nothing, plain
  `secondwind setup` removes a hook an earlier version installed, and `secondwind check` warns while
  one is still there. Use the Claude Code mod instead (below).
- **Offloading depends on where a block came from.** When a caller names the tool and command (MCP
  `optimize` takes optional `tool` and `command`; `exec` uses its own), output that is usually read
  in full, such as a large `cat` or `Read`, stays in view and only inline codecs apply. Callers that
  name nothing behave as before.
- **Figures read from recorded sessions drop.** The trace reader counted images and PDFs as base64
  text and wrapped every tool result in JSON the model never saw. On the author's sessions that
  made tool output look seven times larger than it is, so `cache-savings` and the audit report less.

### Security

- The retired Bash hook let Bash commands run without the user's deny rules or prompts (above).

### Added

- `secondwind eval` replays your recorded sessions through the optimizer and reports, per tool and
  command, what was offloaded, how often the model later needed it, net tokens, and whether every
  block came back intact. `--learn` fits reopen rates on half your sessions, replays the other half
  with and without the gate, and writes `~/.secondwind/policy.json`; `--gate` and `--query` replay
  with the gate or with your latest message as the query; `--blocks` writes one line per result.
- A reopen policy per command class and size decides whether a block may be offloaded. The
  built-in table comes from 1,491 real sessions; a learned `policy.json` replaces it. On held-out
  sessions the learned gate cut reopens per session from 23.6 to 4.8, and no block of 138,209 came
  back different from the original.
- Claude Code mod 0.2.0: sends the command with each block, so a large `cat` stays in view, and
  compresses from 512 bytes instead of 6 KB. For Bash on held-out sessions it saves 9.7% of all
  tool output instead of 6.6%, with fewer extra turns. Tests now pin the pane's off, empty, win and
  loss states.

### Fixed

- `optimize`'s `query` description no longer claims it helps text; it applies to JSON record
  arrays.

## v0.4.0

### Upgrade notes

- **`secondwind exec` runs an argv, not a shell string.** `secondwind exec -- cmd args` runs the
  program directly. A whole pipeline passed as one argument (`secondwind exec "a | b"`) used to go
  through a hidden `sh -c` and now fails with exit 127; pass it with `-c` instead:
  `secondwind exec -c 'a | b'`.
- **`exec` returns the child's exit code.** Any failure used to exit 1. It now exits with the
  command's own code, 128 plus the signal number when a signal ended it, and 127 when the program
  cannot be started.
- **Savings now read net.** `watch`, the web dashboard and the session receipt subtract the tokens
  a `resolve` hands back, so their figures drop wherever offloaded blocks were reopened. They were
  overstated before.
- **`cache-savings` counts tokens.** It counted bytes divided by four; it now counts cl100k tokens
  through the token-priced optimizer, so its totals change.
- Ledgers written by earlier versions load as before.

### Security

- `resolve` accepts only a well-formed marker, sixteen hex digits. A crafted marker could make the
  file store delete a `.blob` file outside the store once it was older than the store's TTL, and
  reached a host-supplied callback store unvalidated. Reported by @idrmn. (#5)

### Fixed

- The Bash hook's rewritten command went through two shell parses, so quoted metacharacters lost
  their quotes and a compound command split. It is now `secondwind exec -c '<command>'`, quoted
  once. (#3)
- `setup --off` with no target re-ran the install. It now removes the `secondwind` MCP server from
  `~/.claude.json` and the hook from `~/.claude/settings.json`, leaving other hooks in place. (#4)
- `exec -c` uses `sh` on Windows, where the `bash` on `PATH` is usually the WSL launcher, and
  falls back from `bash` to `sh` elsewhere. `SECONDWIND_SHELL` overrides both.
- Every resolve is recorded in the ledger with the tokens it returned, and the summary nets them
  out of the totals, the hour, day, week and month buckets, and the curve.
- Percentages keep one decimal, so 99.5 no longer prints as 100.
- `exec` reports its summary line in tokens, the unit every other surface uses.
- The prose digest preview said "not verbatim" though every sentence it shows is verbatim. Its
  label now says so, each run of skipped sentences shows as a `…` line, and a period inside a token
  such as `SKILL.md` no longer ends a sentence.

### Added

- A Claude Code mod in `integrations/claude-code`. It compresses Bash output after the command has
  run, books every resolve, and shows net savings per session in a pane; compression is off in
  each session until turned on there. The repository is its marketplace:
  `/plugin install secondwind --marketplace orchetron/secondwind`. See
  [docs/CLAUDE_CODE.md](docs/CLAUDE_CODE.md).
- `secondwind exec -c <script>` runs a shell string.
- The ledger summary carries `reopens`, `reopened_tokens` and `net_tokens`.

Changes before v0.4.0 are in the git history.
