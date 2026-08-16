//! Terminal session registry. deadlight owns the PTY; dtach owns survival
//! across a deadlight restart. Multiple attachments to one session mirror.
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

pub const MAX_SCROLLBACK: usize = 1_000_000;
pub const MAX_SESSIONS_PER_PROJECT: usize = 16;

/// Session names land in a dtach socket path and a command line. Anything
/// outside this set is a path-traversal or argument-injection vector.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn default_command(project: &str, name: &str) -> Vec<String> {
    if let Ok(c) = std::env::var("DEADLIGHT_CMD") {
        if !c.trim().is_empty() {
            return c.split_whitespace().map(String::from).collect();
        }
    }
    let sock = crate::wsstate::state_dir().join("sock").join(format!("{project}-{name}"));
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    vec![
        "dtach".into(),
        "-A".into(),
        sock.to_string_lossy().into_owned(),
        "-E".into(), // no escape character
        "-r".into(),
        "winch".into(), // repaint full-screen apps on attach
        "-z".into(), // no suspend key
        shell,
        "-l".into(),
    ]
}

pub fn min_geometry(sizes: &HashMap<u64, (u16, u16)>) -> Option<(u16, u16)> {
    let cols = sizes.values().map(|(c, _)| *c).min()?;
    let rows = sizes.values().map(|(_, r)| *r).min()?;
    Some((cols, rows))
}

pub fn push_scrollback(ring: &mut VecDeque<u8>, data: &[u8]) {
    ring.extend(data.iter().copied());
    while ring.len() > MAX_SCROLLBACK {
        ring.pop_front();
    }
}

struct Session {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    scrollback: VecDeque<u8>,
    subs: HashMap<u64, Sender<Vec<u8>>>,
    sizes: HashMap<u64, (u16, u16)>,
    next_id: u64,
}

pub struct Attachment {
    pub id: u64,
    pub key: String,
    pub rx: Receiver<Vec<u8>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Attach to a session, creating it if needed. The new subscriber is sent the
/// scrollback immediately so a reconnecting browser sees where it was.
///
/// Locking discipline: the registry mutex is held only for the short,
/// non-blocking bookkeeping steps (map lookups, inserting a new Session,
/// registering a subscriber). It is never held across a blocking read or
/// write — the pump thread below re-acquires the lock fresh on every loop
/// iteration, only around the read()-independent fan-out, so `attach` can
/// never block behind a PTY that has nothing to say.
pub fn attach(project: &str, name: &str, dir: &Path) -> Result<Attachment, String> {
    if !valid_name(name) {
        return Err("invalid session name".into());
    }
    if !valid_name(project) && project.contains('/') {
        return Err("invalid project name".into());
    }
    let key = format!("{project}-{name}");
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let live_for_project = map.keys().filter(|k| k.starts_with(&format!("{project}-"))).count();
    if !map.contains_key(&key) && live_for_project >= MAX_SESSIONS_PER_PROJECT {
        return Err("too many terminal sessions".into());
    }

    if !map.contains_key(&key) {
        let cmd = default_command(project, name);
        if cmd.is_empty() {
            return Err("empty command".into());
        }
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
        let mut cb = CommandBuilder::new(&cmd[0]);
        cb.args(&cmd[1..]);
        cb.cwd(dir);
        cb.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cb).map_err(|e| e.to_string())?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        map.insert(
            key.clone(),
            Session {
                writer,
                master: pair.master,
                child,
                scrollback: VecDeque::new(),
                subs: HashMap::new(),
                sizes: HashMap::new(),
                next_id: 0,
            },
        );
        let pump_key = key.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                // The blocking read happens with the lock released: only the
                // fan-out after a chunk arrives needs the registry.
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
                        let Some(s) = map.get_mut(&pump_key) else { break };
                        push_scrollback(&mut s.scrollback, &buf[..n]);
                        let chunk = buf[..n].to_vec();
                        s.subs.retain(|_, tx| tx.send(chunk.clone()).is_ok());
                    }
                }
            }
            // PTY closed: drop the session so the next attach respawns it.
            let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(mut s) = map.remove(&pump_key) {
                let _ = s.child.kill();
                let _ = s.child.wait();
            }
        });
    }

    let s = map.get_mut(&key).ok_or("session vanished")?;
    s.next_id += 1;
    let id = s.next_id;
    let (tx, rx) = channel();
    let replay: Vec<u8> = s.scrollback.iter().copied().collect();
    if !replay.is_empty() {
        let _ = tx.send(replay);
    }
    s.subs.insert(id, tx);
    Ok(Attachment { id, key, rx })
}

pub fn write_input(key: &str, data: &[u8]) -> Result<(), String> {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let s = map.get_mut(key).ok_or("no such session")?;
    s.writer.write_all(data).map_err(|e| e.to_string())?;
    s.writer.flush().map_err(|e| e.to_string())
}

pub fn resize(key: &str, id: u64, cols: u16, rows: u16) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = map.get_mut(key) else { return };
    s.sizes.insert(id, (cols, rows));
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

/// Detach only. The PTY keeps running and dtach keeps the session alive, so
/// reopening the same name reattaches.
pub fn detach(key: &str, id: u64) {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let Some(s) = map.get_mut(key) else { return };
    s.subs.remove(&id);
    s.sizes.remove(&id);
    if let Some((c, r)) = min_geometry(&s.sizes) {
        let _ = s.master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names_are_strictly_validated() {
        assert!(valid_name("shell"));
        assert!(valid_name("claude-2"));
        assert!(valid_name("A_b-9"));
        // these land in a socket path and a command line
        assert!(!valid_name(""));
        assert!(!valid_name("../../etc/passwd"));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a;rm -rf /"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(&"x".repeat(33)));
        assert!(valid_name(&"x".repeat(32)));
    }

    #[test]
    fn default_command_wraps_dtach_with_no_ui() {
        let c = default_command("proj", "shell");
        assert_eq!(c[0], "dtach");
        assert!(c.contains(&"-E".to_string()), "no escape character");
        assert!(c.contains(&"-z".to_string()), "no suspend key");
        assert!(c.iter().any(|a| a.contains("proj-shell")), "socket is per project+session");
    }

    #[test]
    fn env_override_replaces_the_command() {
        std::env::set_var("DEADLIGHT_CMD", "cat");
        assert_eq!(default_command("proj", "shell"), vec!["cat".to_string()]);
        std::env::remove_var("DEADLIGHT_CMD");
    }

    #[test]
    fn smallest_attachment_geometry_wins() {
        let mut sizes = HashMap::new();
        sizes.insert(1u64, (100u16, 40u16));
        sizes.insert(2u64, (80u16, 24u16));
        sizes.insert(3u64, (120u16, 50u16));
        assert_eq!(min_geometry(&sizes), Some((80, 24)), "nobody may see clipped output");
        assert_eq!(min_geometry(&HashMap::new()), None);
    }

    #[test]
    fn scrollback_ring_is_bounded() {
        let mut ring = VecDeque::new();
        for _ in 0..(MAX_SCROLLBACK / 10 + 100) {
            push_scrollback(&mut ring, &[b'x'; 10]);
        }
        assert!(ring.len() <= MAX_SCROLLBACK);
    }
}
