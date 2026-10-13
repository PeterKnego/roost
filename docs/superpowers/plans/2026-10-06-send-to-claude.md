# Send to Claude — plan

Spec: [`2026-10-06-send-to-claude-design.md`](../specs/2026-10-06-send-to-claude-design.md).

1. **Menu item.** `treeMenu` offers `Send to Claude` on a file when `claude`
   is launchable or a Claude is running; the choice calls `sendToClaude`.
2. **Existing Claude.** `claudeTarget` picks one by the spec's order;
   `sendToClaude` focuses it and pastes once its socket is open (≤5 s).
3. **New Claude.** `pendingClaudeSend` is armed and `NewTerminal` sent;
   `TerminalStarted` calls `claimClaudeSend`, which waits on
   `whenClaudeReady` (an output tap, `entry.watch`, called from
   `connectTerm`'s `onmessage`) before pasting and focusing.
4. **Test.** `tests/browser/sendclaude.mjs` with a fake `claude` that flushes
   early input, then toggles bracketed paste; revert-check the readiness
   wait, the paste and the target choice.
