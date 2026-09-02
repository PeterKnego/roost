//! Origin / Host validation. The websocket spawns a shell, and WebSocket
//! handshakes are NOT subject to the same-origin policy — a browser will
//! happily let any page connect to ws://127.0.0.1:8444/ws/{project}. Without
//! this check that is drive-by RCE. See spec §Security.
//!
//! The allowlist comes from ROOST_ORIGINS or the *global* config only.
//! Never from {project}/.resh/config.toml: a hostile repo must not be
//! able to allowlist its own domain.
//!
//! **This module is not the rule for every socket.** `src/ide.rs` deliberately
//! inverts it: the socket Claude Code connects to refuses a handshake that
//! *carries* an Origin, and authenticates by lock-file token instead, because
//! its client is a Bun process and a browser is the only thing that would send
//! one there. Do not "restore consistency" by routing that socket through
//! `origin_allowed` — Claude Code's own extensions shipped this socket
//! Origin-blind and unauthenticated through 1.0.23 and that is CVE-2025-52882.
//! See `ide.rs`'s module doc and CLAUDE.md's two Origin constraints.

/// Extract the host portion of an origin like `https://host:8444`.
fn host_of(origin: &str) -> Option<&str> {
    let rest = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    let rest = rest.split('/').next()?;
    if rest.is_empty() {
        return None;
    }
    // IPv6 literal: [::1]:8444
    if let Some(end) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
        return Some(&rest[1..=end]);
    }
    Some(rest.split(':').next().unwrap_or(rest))
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// True when a websocket handshake carrying this Origin may proceed.
/// A missing Origin is rejected: every browser sends one, so its absence
/// means a non-browser client, which has no business here.
pub fn origin_allowed(origin: Option<&str>, allowed: &[String]) -> bool {
    let Some(o) = origin else { return false };
    if allowed.iter().any(|a| a == o) {
        return true;
    }
    host_of(o).map(is_loopback).unwrap_or(false)
}

/// True when an HTTP request's Host may proceed. Behind `tailscale serve` the
/// real hostname arrives as X-Forwarded-Host, so that wins when present; a
/// rebinding attacker's JS cannot set it without a CORS preflight we never
/// answer.
pub fn host_allowed(host: Option<&str>, forwarded: Option<&str>, allowed: &[String]) -> bool {
    let Some(effective) = forwarded.or(host) else {
        return false;
    };
    let Some(h) = host_of(effective) else {
        return false;
    };
    if is_loopback(h) {
        return true;
    }
    allowed.iter().filter_map(|a| host_of(a)).any(|a| a == h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec!["https://box.example.com:8444".to_string()]
    }

    #[test]
    fn loopback_origins_always_pass() {
        assert!(origin_allowed(Some("http://127.0.0.1:8444"), &[]));
        assert!(origin_allowed(Some("http://localhost:8444"), &[]));
        assert!(origin_allowed(Some("http://[::1]:8444"), &[]));
        // any port on loopback — the bind is the boundary, not the port
        assert!(origin_allowed(Some("http://127.0.0.1:9999"), &[]));
    }

    #[test]
    fn configured_origin_passes_exactly() {
        assert!(origin_allowed(Some("https://box.example.com:8444"), &allowed()));
        // scheme and port are part of the origin; near-misses are not matches
        assert!(!origin_allowed(Some("http://box.example.com:8444"), &allowed()));
        assert!(!origin_allowed(Some("https://box.example.com:9999"), &allowed()));
    }

    #[test]
    fn hostile_and_missing_origins_are_rejected() {
        assert!(!origin_allowed(Some("https://evil.example.com"), &allowed()));
        assert!(!origin_allowed(None, &allowed()));
        assert!(!origin_allowed(Some(""), &allowed()));
        // the attack that motivates this module
        assert!(!origin_allowed(Some("https://drive-by.example"), &[]));
        // a subdomain of an allowed host is a different origin
        assert!(!origin_allowed(Some("https://evil.box.example.com:8444"), &allowed()));
    }

    #[test]
    fn host_prefers_forwarded_then_falls_back() {
        // behind tailscale serve: Host is rewritten, X-Forwarded-Host is real
        assert!(host_allowed(
            Some("127.0.0.1:8444"),
            Some("box.example.com:8444"),
            &allowed()
        ));
        // direct loopback browsing, no proxy
        assert!(host_allowed(Some("127.0.0.1:8444"), None, &allowed()));
        assert!(host_allowed(Some("localhost:8444"), None, &[]));
    }

    #[test]
    fn host_rejects_rebinding_and_missing() {
        // DNS rebinding: browser sends the attacker's name as Host
        assert!(!host_allowed(Some("evil.example.com"), None, &allowed()));
        // forwarded wins, so a hostile forwarded value is still checked
        assert!(!host_allowed(Some("127.0.0.1:8444"), Some("evil.example.com"), &allowed()));
        assert!(!host_allowed(None, None, &allowed()));
    }
}
