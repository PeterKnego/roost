# Security

roost's websocket spawns a shell in your project directory. Anyone who can
complete a websocket handshake with a running roost can run commands as the
user it runs as. That is the whole design, and everything below follows from
it.

## The boundary

roost binds `127.0.0.1` only, and that bind is deliberately not configurable.
It has **no authentication of its own**. You are expected to put something
that authenticates in front of it, such as `tailscale serve`, and the
security of a deployment is the security of that layer plus the loopback
boundary.

Inside that boundary, roost defends against the one attacker that loopback
does not stop: **a web page open in the same browser**. Concretely:

- Every browser-facing websocket checks the `Origin` header in its handshake
  against an allowlist and refuses a handshake that carries none. Handshakes
  bypass the same-origin policy, so a socket without this check is remote
  code execution from any page you visit.
- HTTP requests check `Host` and `X-Forwarded-Host` against the same list, to
  defeat DNS rebinding.
- The allowlist comes from the environment or the global config only, never
  from a project's own `.roost/config.toml`, so a repository you clone cannot
  allowlist its own domain.
- HTTP is GET-only apart from `POST /upload` and `POST /paste`, which check
  `Origin` exactly as the websocket does and refuse a request without one.
  Every other state change is a websocket message, so there is no form for a
  hostile page to submit.
- The IDE socket that Claude Code connects to is the one deliberate
  exception. Its client is not a browser and sends no `Origin`, so that
  socket refuses any handshake that *carries* one and authenticates by a
  token from a lock file instead. This is the fix for CVE-2025-52882, and
  the two rules are opposites on purpose.
- Every filesystem path is confined to the project directory by
  canonicalising and prefix-checking before use.
- Session names are restricted to `[A-Za-z0-9_-]{1,32}` because they land in
  a socket path and a command line.

## What counts as a vulnerability

Any way for a page you did not author, or a repository you cloned, to reach
a shell, read or write a file outside the project directory, or change the
allowlist. Also any way for one project to affect another's sessions, and
any way to raise the upload ceiling from a project's own configuration.

Things that are **not** vulnerabilities, because they are the design:

- A roost exposed to a network without an authenticating layer in front.
- Anything reachable only by a process already running as the same user on
  the same host. Such a process can already do everything roost can.
- A shell command run in a session doing what the user asked it to.

## Reporting

Please report privately through GitHub's private vulnerability reporting on
this repository (the **Security** tab, then *Report a vulnerability*), rather
than in a public issue. Include the version or commit, your allowlist and
proxy setup, and the steps to reproduce.

This is a one-person project with no security team and no bounty. You will
get an acknowledgement, a fix or a reasoned explanation, and credit in the
release notes if you want it.

## Supported versions

Only the latest tagged release and `master` receive fixes.
