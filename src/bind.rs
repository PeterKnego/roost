//! Which address the server binds, and the one deployment allowed to widen it.
//!
//! CLAUDE.md's first hard constraint is that roost binds `127.0.0.1` and that
//! the loopback bind *is* the security boundary, because the websocket spawns a
//! shell. That is true of a host, and it cannot be true of a container: inside
//! one, `127.0.0.1` is the *container's* loopback, so a host publishing
//! `127.0.0.1:8123:8123` reaches the process through the bridge address and a
//! loopback-bound roost is unreachable.
//!
//! So the container **substitutes** the network namespace for the loopback
//! bind. A substitution, not a relaxation, and it only holds with all three of:
//! the port published to the host's loopback only (never `0.0.0.0:8123:8123`,
//! which most daemons also punch through the host firewall); no other reachable
//! service on the container's network, because the browser-facing socket's
//! `Origin` check stops a client that *sends* one and a curl from a second
//! container sends none; and this gate.
//!
//! Three properties here are load-bearing.
//!
//! **It is not an address.** `ROOST_BIND=0.0.0.0` is a thing someone types on a
//! host — it reads like a preference, and it is drive-by RCE. A boolean named
//! for its consequence can only be set on purpose.
//!
//! **An unrecognised value is fatal, not a fallback.** Falling back to loopback
//! is the *safe* direction and the wrong one: `ROOST_BIND_ALL=true` in a
//! compose file would produce a container that starts, listens where nothing
//! can reach it, and says nothing about why. That is the shape of every row in
//! CLAUDE.md's "absence of evidence" table — a check that failed, read as an
//! answer. Here "I do not understand this" is a third outcome, and it exits.
//!
//! **Environment only, never the config file.** `allowed_origins` is
//! global-config-only so a cloned repository cannot widen a boundary; a bind
//! gate has the same shape and needs it more, because the global config lives
//! on a volume the container's own user can write. There is deliberately no
//! TOML key, and adding one would undo the reason this module exists.
//!
//! `ide.rs` is untouched: its client is a `claude` running in the same
//! container, so container loopback is exactly right for it, and it
//! authenticates by token besides.

/// Where the listener goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bind {
    Loopback,
    /// Every interface in this network namespace. Only reachable through
    /// `ROOST_BIND_ALL=1`, and only sound where the namespace is the boundary.
    AllInterfaces,
}

impl Bind {
    pub fn addr(self) -> &'static str {
        match self {
            Bind::Loopback => "127.0.0.1",
            Bind::AllInterfaces => "0.0.0.0",
        }
    }
}

pub const VAR: &str = "ROOST_BIND_ALL";

/// What `ROOST_BIND_ALL` means, or why the process must not start.
///
/// `None` and an empty value are both "unset": `export ROOST_BIND_ALL=` is how
/// a shell clears a variable, and a cleared variable is an operator saying no.
/// Only whitespace around a value is tolerated — a compose file can carry it —
/// and nothing else is guessed at.
pub fn decide(raw: Option<&str>) -> Result<Bind, String> {
    match raw.map(str::trim) {
        None | Some("") | Some("0") => Ok(Bind::Loopback),
        Some("1") => Ok(Bind::AllInterfaces),
        Some(other) => Err(format!(
            "roost: {VAR}={other:?} is not a value I understand — it is `1` (bind every \
             interface, for a container whose network namespace is the boundary) or `0`/unset \
             (bind 127.0.0.1). Refusing to start rather than guessing: guessing loopback would \
             leave a container listening where nothing can reach it, with no word about why."
        )),
    }
}

/// The line printed when the substitution is in force, naming what was traded
/// rather than only the address. It is the one place an operator reading
/// `docker logs` learns that the loopback boundary is gone and what now
/// replaces it.
pub fn notice() -> String {
    format!(
        "roost: {VAR}=1 — binding 0.0.0.0. The loopback bind is NOT the security boundary here; \
         the network namespace is. This is sound only if the port is published to the host's \
         loopback (127.0.0.1:PORT:PORT) and nothing else on this container's network can reach \
         it. A websocket here spawns a shell."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole table, one row per assertion, so a row that changes fails by
    /// name.
    ///
    /// Revert-checked: making the `Some(other)` arm `Ok(Bind::Loopback)` — the
    /// "be forgiving" version — fails `an_unrecognised_value_refuses_to_start`
    /// below rather than this one, which is the point of splitting them.
    #[test]
    fn only_an_explicit_one_widens_the_bind() {
        assert_eq!(decide(None), Ok(Bind::Loopback), "unset is the historical behaviour");
        assert_eq!(decide(Some("0")), Ok(Bind::Loopback));
        assert_eq!(decide(Some("")), Ok(Bind::Loopback), "a cleared variable is an operator saying no");
        assert_eq!(decide(Some("   ")), Ok(Bind::Loopback));
        assert_eq!(decide(Some("1")), Ok(Bind::AllInterfaces));
        assert_eq!(decide(Some(" 1 ")), Ok(Bind::AllInterfaces), "a compose file may carry whitespace");
        assert_eq!(Bind::Loopback.addr(), "127.0.0.1");
        assert_eq!(Bind::AllInterfaces.addr(), "0.0.0.0");
    }

    /// The third outcome. Every one of these is a plausible thing to write in a
    /// compose file, and each would otherwise start a container that listens
    /// where nothing can reach it and reports success.
    ///
    /// Revert-checked: with the `Some(other)` arm returning `Ok(Loopback)`,
    /// this fails on `"true"` with `Ok(Loopback)` against the expected `Err`.
    #[test]
    fn an_unrecognised_value_refuses_to_start() {
        for bad in ["true", "yes", "TRUE", "on", "0.0.0.0", "2", "1 1"] {
            let got = decide(Some(bad));
            assert!(got.is_err(), "{bad:?} must not be guessed at, got {got:?}");
            // On *why*, not merely `is_err`: the message is the whole value of
            // refusing, and one that did not name the offending value would
            // leave an operator grepping a compose file for it.
            let msg = got.unwrap_err();
            assert!(msg.contains(bad), "the message must name the value: {msg}");
            assert!(msg.contains(VAR), "and the variable: {msg}");
        }
    }

    /// An address-shaped value is refused like any other, which is the whole
    /// reason this is a boolean. Someone reaching for `ROOST_BIND=0.0.0.0`
    /// muscle memory must not find it working here.
    #[test]
    fn an_address_is_not_a_value_this_accepts() {
        assert!(decide(Some("0.0.0.0")).is_err());
        assert!(decide(Some("::")).is_err());
        assert!(decide(Some("127.0.0.1")).is_err(), "even the safe one — it is not an address field");
    }

    /// The notice has to say what was traded, not just what was bound. An
    /// operator scanning `docker logs` for why their shell is reachable gets
    /// one line, and it is this one.
    #[test]
    fn the_notice_names_the_substitution_and_not_only_the_address() {
        let n = notice();
        assert!(n.contains("0.0.0.0"));
        assert!(n.contains("network namespace"), "what replaced the boundary: {n}");
        assert!(n.contains("127.0.0.1:PORT:PORT"), "and what the operator now owes it: {n}");
        assert!(n.contains("spawns a shell"), "and why it matters: {n}");
    }
}
