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

This has not been exercised against a real Claude Code `Stop` hook — the
`/dev/tty` write is this design's one inferred assumption, and a subagent
invocation has no controlling terminal to write to. Treat the hook config
above as the intended usage, not a confirmed one.

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

None of the browser-side behaviour — the OS notification actually appearing,
click focusing the right tab and terminal, the badge clearing — has been
exercised in a real browser yet. It is implemented against the design, not
verified end to end.

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
