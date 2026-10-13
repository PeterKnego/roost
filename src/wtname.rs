//! Worktree names. roost and Claude Code put worktrees in the same place, so
//! they use one convention — `worktree-<name>` for the directory and the
//! branch — and a name from either side passes the same gate before it
//! reaches a path or a `git` argument. A leading `-` would read as an option
//! and a leading `_` buys nothing, so both are refused.

const RULE: &str = "^[A-Za-z0-9][A-Za-z0-9_-]{0,31}$";
const MAX_TRIES: u32 = 64;

// Small on purpose: three words from these give 16*16*16 names, and the
// generator only needs to avoid what this one repository already has.
const ADJ: [&str; 16] = [
    "amber", "brave", "bright", "calm", "clever", "eager", "gentle", "golden",
    "lucky", "mellow", "nimble", "quiet", "rapid", "silent", "steady", "swift",
];
const VERB: [&str; 16] = [
    "dancing", "drifting", "flying", "gliding", "humming", "jumping", "leaping", "racing",
    "roaming", "rolling", "running", "sailing", "singing", "soaring", "spinning", "wandering",
];
const NOUN: [&str; 16] = [
    "badger", "comet", "falcon", "fox", "heron", "lynx", "maple", "otter",
    "panda", "pebble", "raven", "river", "spruce", "tiger", "walrus", "willow",
];

pub fn valid(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

pub fn check(name: &str) -> Result<(), String> {
    if valid(name) { Ok(()) } else { Err(format!("worktree name {name:?} must match {RULE}")) }
}

pub fn dir_name(name: &str) -> String {
    format!("worktree-{name}")
}

/// A fresh `adjective-verb-noun`, skipping any name `taken` reports. Seeded
/// from the clock and pid: uniqueness comes from `taken`, not from the seed.
pub fn generate(taken: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64).rotate_left(32);
    for _ in 0..MAX_TRIES {
        // xorshift64: no dependency, and quality is irrelevant here.
        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
        let n = format!("{}-{}-{}", ADJ[(x & 15) as usize], VERB[((x >> 4) & 15) as usize], NOUN[((x >> 8) & 15) as usize]);
        if !taken(&n) {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_names_that_could_be_an_argument_or_a_path() {
        // Revert check: when b[0].is_ascii_alphanumeric() is changed to `true`,
        // this test fails with `called Result::unwrap_err() on an Ok value: ()`
        // at line 67:32, failing on "--foo" because check("--foo") returns Ok(())
        // instead of Err, confirming that the leading dash check is essential.
        for bad in ["", "--foo", "-x", "_x", "../x", "a/b", "a b", "a.b", &"a".repeat(33)] {
            let e = check(bad).unwrap_err();
            assert!(e.contains("[A-Za-z0-9][A-Za-z0-9_-]"), "message must quote the rule, got {e:?} for {bad:?}");
        }
    }

    #[test]
    fn accepts_claude_codes_own_shapes() {
        for good in ["bright-running-fox", "Foo-bar_2", "a", &"a".repeat(32)] {
            assert!(check(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn generator_skips_taken_names_and_gives_up_when_all_are_taken() {
        let first = generate(&|_| false).unwrap();
        assert!(valid(&first) && first.split('-').count() == 3, "{first}");
        let again = generate(&|n| n == first).unwrap();
        assert_ne!(again, first);
        assert!(generate(&|_| true).is_none());
    }

    #[test]
    fn dir_name_is_claude_codes_prefix() {
        assert_eq!(dir_name("bright-fox"), "worktree-bright-fox");
    }
}
