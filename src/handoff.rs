//! A per-project message log that a browser agent and a roost Claude share.
//!
//! Claude in Chrome cannot be given tools of ours, but it reads and fills web
//! pages; a roost Claude cannot see a web page, but it calls tools. The
//! handoff is the one surface both can reach: a plain page for the browser
//! side (`/frag/{project}/handoff`), `handoff_read`/`handoff_send` for the
//! Claude side (`browser.rs`, `mcp.rs`).
//!
//! In memory and bounded, never persisted: it is a conversation channel, not
//! a record. A restart drops it, the same way it drops a parked proposal.
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Oldest messages fall off past this many per project.
pub const MAX_MESSAGES: usize = 200;
/// One message's text. Enough for a page excerpt or a stack trace; more than
/// that belongs in a file the other side can open.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;
/// The longest `handoff_read` may wait. Inside `roost mcp`'s own call budget
/// so the server always answers before the client gives up.
pub const MAX_WAIT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    /// Increasing per process, never reused, so `since` stays meaningful
    /// after older messages have been dropped.
    pub id: u64,
    /// Unix seconds.
    pub at: u64,
    /// `browser`, or `claude:<session>`.
    pub from: String,
    pub text: String,
}

struct Logs {
    by_project: HashMap<String, VecDeque<Message>>,
    next_id: u64,
}

fn logs() -> &'static (Mutex<Logs>, Condvar) {
    static L: OnceLock<(Mutex<Logs>, Condvar)> = OnceLock::new();
    // Ids start at the start time in milliseconds rather than at 1, so a
    // reader holding a `since` from before a restart is not stuck waiting
    // behind ids that count up from 1 again.
    L.get_or_init(|| {
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1);
        (Mutex::new(Logs { by_project: HashMap::new(), next_id: start }), Condvar::new())
    })
}

fn lock() -> std::sync::MutexGuard<'static, Logs> {
    logs().0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Appends a message and wakes every waiting reader. Refuses empty and
/// oversized text with a reason the sender is shown, rather than truncating:
/// half a message read as the whole is worse than none.
pub fn post(project: &str, from: &str, text: &str) -> Result<Message, String> {
    if text.trim().is_empty() {
        return Err("empty message".into());
    }
    if text.len() > MAX_TEXT_BYTES {
        return Err(format!("message is {} bytes, over the {MAX_TEXT_BYTES} byte limit", text.len()));
    }
    let msg = {
        let mut l = lock();
        let id = l.next_id;
        l.next_id += 1;
        let msg = Message { id, at: crate::errlog::now_secs(), from: from.to_string(), text: text.to_string() };
        let log = l.by_project.entry(project.to_string()).or_default();
        log.push_back(msg.clone());
        while log.len() > MAX_MESSAGES {
            log.pop_front();
        }
        msg
    };
    logs().1.notify_all();
    Ok(msg)
}

/// Every message in `project` with an id above `since`.
pub fn since(project: &str, since: u64) -> Vec<Message> {
    let l = lock();
    l.by_project.get(project).map(|v| v.iter().filter(|m| m.id > since).cloned().collect()).unwrap_or_default()
}

/// `since`, but waits up to `wait` (capped at [`MAX_WAIT`]) for a message
/// that is not `skip_from`'s own, so a Claude polling for an answer is not
/// woken by the question it just sent. The condvar wait releases the lock;
/// nothing else is held across it.
pub fn wait_since(project: &str, since_id: u64, skip_from: &str, wait: Duration) -> Vec<Message> {
    let deadline = Instant::now() + wait.min(MAX_WAIT);
    let (_, cv) = logs();
    let mut l = lock();
    loop {
        let got: Vec<Message> = l
            .by_project
            .get(project)
            .map(|v| v.iter().filter(|m| m.id > since_id).cloned().collect())
            .unwrap_or_default();
        let now = Instant::now();
        if got.iter().any(|m| m.from != skip_from) || now >= deadline {
            return got;
        }
        l = cv.wait_timeout(l, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_over_the_cap_is_refused_and_says_why() {
        let e = post("ho-cap", "browser", &"x".repeat(MAX_TEXT_BYTES + 1)).unwrap_err();
        assert!(e.contains("byte limit"), "{e}");
        assert!(since("ho-cap", 0).is_empty(), "nothing was stored");
        assert!(post("ho-cap", "browser", "  \n").unwrap_err().contains("empty"));
    }

    #[test]
    fn the_log_keeps_only_the_newest_messages_and_ids_keep_counting() {
        for i in 0..MAX_MESSAGES + 5 {
            post("ho-ring", "browser", &format!("m{i}")).unwrap();
        }
        let all = since("ho-ring", 0);
        assert_eq!(all.len(), MAX_MESSAGES);
        assert_eq!(all[0].text, "m5", "the five oldest fell off");
        assert!(all.windows(2).all(|w| w[0].id < w[1].id));
        let last = all.last().unwrap().id;
        assert!(since("ho-ring", last).is_empty());
    }

    #[test]
    fn projects_do_not_see_each_other() {
        post("ho-a", "browser", "for a").unwrap();
        assert!(since("ho-b", 0).is_empty());
    }

    /// Revert-checked: with `notify_all` removed from `post`, the reader
    /// sleeps its whole two seconds and fails on the elapsed bound.
    #[test]
    fn a_waiting_reader_wakes_on_a_post_and_ignores_its_own() {
        let start = Instant::now();
        let reader = std::thread::spawn(|| wait_since("ho-wait", 0, "claude:t", Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(100));
        post("ho-wait", "claude:t", "my own question").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        post("ho-wait", "browser", "the answer").unwrap();
        let got = reader.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(1500), "woke on the post, took {:?}", start.elapsed());
        assert_eq!(got.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["my own question", "the answer"]);
    }

    #[test]
    fn a_wait_with_nothing_new_returns_empty_at_the_deadline() {
        let start = Instant::now();
        assert!(wait_since("ho-idle", 0, "x", Duration::from_millis(150)).is_empty());
        assert!(start.elapsed() >= Duration::from_millis(150));
    }
}
