# Notifications

roost raises an OS notification when something in a terminal wants your
attention — Claude finishing a task, or asking for a decision — and clicking
it focuses the browser tab and terminal that fired.

## Triggering one

Any process in a roost terminal can fire one. Three equivalent ways:

```bash
roost notify "Build done" "42 tests passed"
printf '\033]777;notify;Build done;42 tests passed\007'
printf '\033]9;done\007'          # iTerm2 form: body only, title defaults to the session name
```

The sequences are the ones urxvt/kitty/dunst and iTerm2 already use, so
anything that can already notify those notifies roost unchanged. `ST` may
be `BEL` (`\007`) or `ESC \`; in the `777` form only the first three `;` are
structural (separating `777`, `notify`, title, and body), so a body
containing `;` needs no escaping.

`roost notify` writes to `/dev/tty`, falling back to stdout only when
stdout is *itself a terminal*. A missing title is a usage error (exit 2); having
no terminal to write to at all is a loud failure (exit 1) rather than a silent
no-op — a misconfigured hook that did nothing would otherwise look exactly like
a hook that never fired. The fallback deliberately does not accept a pipe:
nothing on the other end of one can interpret an escape sequence.

Title and body are sanitised and capped (control characters stripped, 100 and
500 characters respectively — the same rules the parser applies on the way
in) before the sequence is emitted, and a `;` in the title is replaced with
`,`. This is what lets multi-line tool output or ANSI-coloured output pass
through `roost notify "$title" "$(some_command)"` and still produce a
notification: unsanitised, a newline or an escape code in the body would have
made the parser abandon the sequence outright, silently, with exit status 0.

## Discovering that it is available

Every terminal roost spawns carries:

| Variable | Meaning |
|---|---|
| `ROOST_NOTIFY` | `1` when notifications are available |
| `ROOST_PROJECT` | the project this terminal belongs to |
| `ROOST_SESSION` | this terminal's session name |

So a script — or a model — can check `[ -n "$ROOST_NOTIFY" ]` before
trying.

## Firing automatically from Claude Code

Open the bell in a project's workspace. Its first row says whether Claude
notifications are on for that project and offers the switch. Enabling
writes two hook entries into the project's `.claude/settings.local.json`
(the personal, gitignored one; the committed `settings.json` and the
global `~/.claude/settings.json` are never touched), each running
`roost claude-hook`:

```json
{
  "hooks": {
    "Notification": [ { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] } ],
    "Stop":         [ { "hooks": [ { "type": "command", "command": "roost claude-hook", "timeout": 5 } ] } ]
  }
}
```

Disabling removes exactly those entries and nothing else; other hooks,
other keys and their order survive. The first time roost replaces an
existing file it copies it to `settings.local.json.bak` and never
overwrites that copy. A file roost cannot parse shows on the bell as
"cannot tell" and is never written.

`roost claude-hook` reads the event Claude Code pipes on stdin and raises:

| Event | Title | Body |
|---|---|---|
| `Notification` · `permission_prompt` | Claude needs you | wants permission to run a tool |
| `Notification` · `idle_prompt` | Claude needs you | is waiting for your input |
| `Notification` · `agent_needs_input` | Claude needs you | an agent needs your input |
| `Notification` · `elicitation_dialog`, `elicitation_url_dialog` | Claude needs you | is asking a question |
| `Stop` | Claude finished | the first line of Claude's last message |

Anything else is ignored. Unlike `roost notify`, this command always exits
0 and is silent when `ROOST_NOTIFY` is unset: the same project is used
outside roost, and a `Stop` hook that exits non-zero shows an error in
every such session. The snippet above is also what to paste by hand into
any other settings file if you want the hooks somewhere roost will not
write.

**Existing hooks and scripts must be updated by hand.** A
`.claude/settings.json` written before 2026-09-02 still says `resh notify`
(or `deadlight notify`, before 2026-08-18), and a script gating on
`$RESH_NOTIFY` still checks that name. Once the old binary is removed,
`resh notify` is just a missing command, and the terminal only ever exports
`ROOST_NOTIFY` now — so `$RESH_NOTIFY` is always unset and the script's
guard silently skips it. Either way the hook does nothing, and a hook that
does nothing looks exactly like a hook that never fired — the same failure
this command's loud-failure design exists to prevent. Which replacement
command depends on what called `resh notify`: a hand-written script — one
that passed a title and body as arguments, `resh notify "Build" "done"` —
becomes `roost notify` with the same arguments; a Claude Code `Stop` or
`Notification` hook entry (the JSON-on-stdin form Claude Code itself
invokes) becomes `roost claude-hook` instead. Pointing a hand-written,
argument-passing call at `roost claude-hook` is the same silent no-op this
paragraph warns about: it ignores argv, reads stdin instead, and exits 0
whether or not stdin holds anything it recognises. Either way, also update
the variable check to `ROOST_NOTIFY` in every hook and script written
against an old name. A host may leave a `resh` symlink to the `roost`
binary so an old hook's *command* keeps working; nothing rescues an old
`$RESH_NOTIFY` *guard* in a terminal opened after the rename.

**The `/dev/tty` write is confirmed** (2026-08-17), including the shape a hook
actually runs in: inside a resh terminal with stdout captured by a pipe,
`resh notify` still reaches the terminal and the notice is delivered.

**A process with no terminal anywhere now fails loudly.** That is the subagent
case: no `/dev/tty`, and stdout a pipe rather than a terminal. It used to write
the sequence into that pipe and exit **0** — a silent no-op wearing a success
code, which is the exact failure this command exists to prevent, since an OSC
sequence in a pipe has no terminal to interpret it. Now:

```
$ roost notify "Claude" "finished"     # no /dev/tty, stdout a pipe
roost notify: no controlling terminal, and stdout is not one either —
nothing would read the sequence, so no notification was sent. This is what a
hook invoked without a terminal (e.g. a subagent) looks like.
exit=1
```

The stdout fallback is unchanged where it was ever meaningful: a process whose
stdout *is* a terminal still uses it. Only the case where nothing could possibly
read the sequence became an error.

## In the browser

A bell in the header shows an unread count across **all** projects, and the
same count prefixes the browser tab title so it is visible from a background
tab; the favicon gets a small red-dot badge (not the count itself) when there
is anything unread. Notices persist across a roost restart, so one raised
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

**The OS notification and the click routing are verified too**, with the
permission actually granted (via CDP, in a throwaway Chromium profile) rather
than skipped:

- The service worker registers **and activates**, and `showNotification` really
  produces a notification object. Fired from a session whose payload claimed a
  different project, it came back as
  `{title: "projA · shell", body: "projB — spoof attempt", tag: "projA/shell"}`
  — so the attribution rule holds in the OS banner's most prominent field, which
  is the whole point of it.
- Two sessions' notifications coexist under distinct tags (`projA/shell`,
  `projB/shell`), so the per-session `tag` replaces a session's own banner
  without collapsing different ones.
- Click routing: `clients.matchAll` finds the window whose decoded pathname
  matches the notice's project, the page receives
  `{kind: "focus", project, session}`, and `focusSession` then marks that
  session's notices read on the server (3 unread → 0) and activates its terminal
  tab. With only a `/projA` window open, a `projB` notice correctly takes the
  other branch instead — reuse a window and navigate it to
  `/projB#session=shell`.

**What still cannot be proven by automation:** the browser dispatching a real
`notificationclick` from a real desktop click, and `client.focus()` succeeding.
A synthetically constructed `NotificationEvent` cannot call `waitUntil` (it
throws, aborting the handler before the routing), and `focus()` needs user
activation — in headless it raises `InvalidAccessError`. Both are browser-side
preconditions rather than roost logic, and the code they gate is exercised
above.

**The 100-notice cap is verified end to end.** 120 notices were fired through
real PTYs — 10 from each of 12 sessions, since the limiter is per *session*, so
firing concurrently takes seconds rather than the twelve minutes one session
would need. Exactly 100 were retained, with ids 21..120: the oldest 20 dropped
and the newest kept, the retained window contiguous and monotonic. The bell and
tab title both read 100, and a restart reloaded the same capped window
unchanged.

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
the project and session roost itself observed, never to anything the
message claims about itself.
