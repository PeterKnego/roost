# Send to Claude from the tree

*2026-10-06. Issue [#146](https://github.com/PeterKnego/roost/issues/146),
which carries the shape. This records the decisions implementation had to
settle, and the measurements behind the two that are not obvious.*

## What it does

The tree's right-click menu on a file offers **Send to Claude**. It puts
`@<path> ` into a Claude terminal's prompt, focuses that terminal, and submits
nothing — the user goes on writing the message around the path.

Client-only. No intent, no server change: the server already says which
terminals run a Claude (`claude_sessions`) and already starts one
(`NewTerminal { launch: claude }`).

## Which Claude

The terminal last focused, if it runs a Claude; else a Claude terminal that is
an active tab in some pane; else any terminal tab running one; else a live
session the server counts as a Claude that has no tab (`focusSession` opens
one). "Runs a Claude" is `state.claude_sessions`, never the session name —
`claude` is a legal name for a plain shell.

None: a new Claude terminal, through the same `NewTerminal` the ✻ button
sends. So the worktree prompt still guards a project where roost has evidence
of a Claude it has no tab for; the pending send waits for whichever
`TerminalStarted` follows, and expires unclaimed if the user goes to a
worktree instead.

## Pasted, not typed

Claude Code answers a permission prompt on a bare digit. A *typed*
`@src/v2.rs` arriving while one is up could pick option 2. `term.paste` sends
a bracketed paste whenever the app has enabled the mode — Claude Code always
has — and a paste only ever reaches the input. It also goes through `onData`,
so it inherits the dead-socket guard every keystroke has.

A path with whitespace is sent as `@"path"`.

## When a new Claude is listening

Observed, not timed: start-up runs from half a second to a self-update.
Measured against a real `claude` on the deploy host, launched the way roost
launches it (`claude\r` typed at spawn):

```
0.00s  ?2004l                      bash accepts the line
0.34s  ?2004h ?2031h ?1004h        Claude enables paste
0.61s  ?2004l ?2031l ?1004l        ... and disables it again
0.73s  ?2004h ?2031h ?1004h        ... and re-enables it
0.78s  1047 bytes                  the UI
```

Input sent at the first `?2004h` (+0.2 s) is lost; sent 0.5 s or more after
it, it lands. So: **a `?2004h` that follows a `?2004l`, then 500 ms of no
output.** Following an `l` is what skips the shell's own prompt, which enables
the mode first; and since Claude Code produces an `l`/`h` pair itself, a shell
that never touches the mode still arms it. Across three runs of exactly this
rule against a real Claude, it fired at 1.5–1.7 s and the paste landed every
time.

30 s without that, and nothing is pasted; a banner says so. Text dropped into a
shell, or into whatever a stalled start-up is showing, is worse than not
sending.

## Known limits

- **A project Claude has never trusted** shows its trust dialog first. That
  dialog satisfies the readiness rule, and the paste goes to the dialog, which
  ignores it. The user sees the dialog and the path is not in the prompt.
  Detecting the dialog would mean parsing Claude's screen, which is exactly
  the kind of guess the rest of this avoids.
- Folders are not offered.
