# Notifications

deadlight raises an OS notification when something in a terminal wants your
attention — Claude finishing a task, or asking for a decision — and clicking
it focuses the browser tab and terminal that fired.

## Triggering one

Any process in a deadlight terminal can fire one. Three equivalent ways:

```bash
deadlight notify "Build done" "42 tests passed"
printf '\033]777;notify;Build done;42 tests passed\007'
printf '\033]9;done\007'          # iTerm2 form: body only, title defaults to the session name
```

The sequences are the ones urxvt/kitty/dunst and iTerm2 already use, so
anything that can already notify those notifies deadlight unchanged. `ST` may
be `BEL` (`\007`) or `ESC \`; in the `777` form only the first three `;` are
structural (separating `777`, `notify`, title, and body), so a body
containing `;` needs no escaping.

`deadlight notify` writes to `/dev/tty`, falling back to stdout if there is no
controlling terminal. A missing title is a usage error (exit 2); no
controlling terminal and no usable stdout is a loud failure (exit 1) rather
than a silent no-op — a misconfigured hook that did nothing would otherwise
look exactly like a hook that never fired.

Title and body are sanitised and capped (control characters stripped, 100 and
500 characters respectively — the same rules the parser applies on the way
in) before the sequence is emitted, and a `;` in the title is replaced with
`,`. This is what lets multi-line tool output or ANSI-coloured output pass
through `deadlight notify "$title" "$(some_command)"` and still produce a
notification: unsanitised, a newline or an escape code in the body would have
made the parser abandon the sequence outright, silently, with exit status 0.

## Discovering that it is available

Every terminal deadlight spawns carries:

| Variable | Meaning |
|---|---|
| `DEADLIGHT_NOTIFY` | `1` when notifications are available |
| `DEADLIGHT_PROJECT` | the project this terminal belongs to |
| `DEADLIGHT_SESSION` | this terminal's session name |

So a script — or a model — can check `[ -n "$DEADLIGHT_NOTIFY" ]` before
trying.

## Firing automatically from Claude Code

The intended usage is a `Stop`/`Notification` hook in the project's
`.claude/settings.json`, so you hear about a finished turn or a pending
permission prompt without watching the tab:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "deadlight notify \"Claude\" \"finished\"" }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "deadlight notify \"Claude\" \"needs your input\"" }] }
    ]
  }
}
```

**The `/dev/tty` write is confirmed** (2026-08-17), including the shape a hook
actually runs in: inside a deadlight terminal with stdout captured by a pipe,
`deadlight notify` still reaches the terminal and the notice is delivered.

**A process with no controlling terminal at all is still a silent no-op.** That
is the subagent case. With no `/dev/tty` and stdout captured, the sequence is
written to that captured stdout, nothing is delivered, and the exit status is
**0**:

```
$ deadlight notify "Claude" "finished"     # no /dev/tty, stdout a pipe
exit=0
stdout=^[]777;notify;Claude;finished^G
```

This meets the rule stated below — a pipe *is* a usable stdout — but not its
intent, since an escape sequence written into a pipe can never produce a
notification, making failure indistinguishable from success. If you wire a hook
that may run detached, verify it fires rather than trusting its exit status.

## In the browser

A bell in the header shows an unread count across **all** projects, and the
same count prefixes the browser tab title so it is visible from a background
tab; the favicon gets a small red-dot badge (not the count itself) when there
is anything unread. Notices persist across a deadlight restart, so one raised
overnight is still there in the morning.

OS notifications need a secure context — `localhost` or `tailscale serve`
HTTPS. Over plain `http://` to a tailnet IP the notice panel still works but
the OS cannot be asked. Permission is requested from the panel's own button,
never automatically on load.

The OS notification's title is always `project · session` — server truth,
the same attribution the in-page panel shows — never the payload's own
title, which goes in the body alongside the payload's body instead. This is
what stops a hostile payload (`cat` of a file containing a forged OSC
sequence) from producing a banner that looks like it came from a different
project.

**Verified in a real browser** on 2026-08-17, driving input through the actual
terminal websocket so the whole path — PTY, OSC parser, notice store, hub
broadcast, client — ran for real: the unread count in the bell and the tab
title, the favicon badge, the panel's `project · session` attribution, a
spoofing payload being attributed to its true origin anyway, the iTerm2 `OSC 9`
form defaulting its title to the session, a `;` inside a body surviving intact,
the 10-per-minute limit and its `(N suppressed)` accounting, mark-all-read
clearing the badges while keeping history, and 18 notices with 14 unread
surviving a restart with badges and panel restored.

**Two things remain unverified**: the OS banner actually rendering, and
click-to-focus. Both sit behind a permission grant that needs a real gesture on
the browser's own prompt — the code path feeding them is exercised, the OS-level
render is not. The 100-notice retention cap is also untested.

## Limits

| Thing | Limit |
|---|---|
| Retained notices | 100 |
| Title / body | 100 / 500 characters |
| Per session | 10 per minute, then suppressed and counted |

A session that goes over its per-minute limit does not lose notices silently:
the next notice admitted once the window resets has the suppressed count
appended to its body, e.g. `"...(3 suppressed)"`.

Text arriving over a terminal is untrusted — `cat` of a hostile file could
emit one — so it is stripped of control characters and always attributed to
the project and session deadlight itself observed, never to anything the
message claims about itself.
