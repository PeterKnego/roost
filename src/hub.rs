//! One Hub per project: owns the Workspace, the subscriber list, and the
//! dispatch from intent to either a pure transition or a file operation.
//! Everything the sockets do goes through here, so mirroring is automatic.
use crate::proto::{Event, Intent};
use crate::workspace::{self, Workspace};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

pub type ConnId = String;

pub struct Hub {
    pub project: String,
    pub dir: std::path::PathBuf,
    pub ws: Workspace,
    pub subs: HashMap<ConnId, Sender<String>>,
    next_id: u64,
    /// Paths deadlight itself just wrote, with the resulting hash. The watcher
    /// (Task 8) drops matching events so a save does not echo back.
    pub self_writes: HashMap<String, u64>,
    /// Set once a filesystem watcher has been spawned for this hub, so
    /// `for_project` starts at most one watcher per project even though it
    /// runs on every connection.
    pub watching: bool,
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<Hub>>>>> = OnceLock::new();

impl Hub {
    pub fn new(project: &str, dir: std::path::PathBuf) -> Hub {
        let (ws, warn) = crate::wsstate::load(project);
        if let Some(w) = warn {
            eprintln!("deadlight: {w}");
        }
        Hub {
            project: project.to_string(),
            dir,
            ws,
            subs: HashMap::new(),
            next_id: 0,
            self_writes: HashMap::new(),
            watching: false,
        }
    }

    /// One hub per project, shared by every connection to it. Also the place
    /// a project's filesystem watcher is started: the first connection to
    /// see a fresh hub spawns it, and `watching` makes that idempotent so a
    /// second connection racing in does not start a second watcher.
    pub fn for_project(project: &str, dir: std::path::PathBuf) -> Arc<Mutex<Hub>> {
        let reg = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
        let mut map = reg.lock().unwrap_or_else(|e| e.into_inner());
        let arc = map
            .entry(project.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(Hub::new(project, dir.clone()))))
            .clone();
        // The registry lock is dropped by the time we get here (this is
        // after the `entry` call completes, and `map` is not touched again),
        // so locking the hub next cannot deadlock against another thread
        // that holds the hub lock and wants the registry lock.
        drop(map);
        if !Hub::lock(&arc).watching {
            let ms: u64 = std::env::var("DEADLIGHT_DEBOUNCE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300);
            // Spawned with no hub lock held: walking the tree and registering
            // watches touches the filesystem, and that must never happen
            // while blocking every other connection to this project.
            let ok = crate::watch::spawn(project, dir, arc.clone(), std::time::Duration::from_millis(ms));
            let mut h = Hub::lock(&arc);
            h.watching = true;
            h.ws.watch_degraded = !ok;
        }
        arc
    }

    /// Lock a hub, recovering from poisoning. A panic in one connection thread
    /// must not take the project down for every other browser.
    pub fn lock(h: &Arc<Mutex<Hub>>) -> std::sync::MutexGuard<'_, Hub> {
        h.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn subscribe(&mut self) -> (ConnId, Receiver<String>) {
        self.next_id += 1;
        let id = format!("c{}", self.next_id);
        let (tx, rx) = channel();
        self.subs.insert(id.clone(), tx);
        (id, rx)
    }

    pub fn unsubscribe(&mut self, id: &ConnId) {
        self.subs.remove(id);
    }

    /// Send to everyone; prune receivers that have gone away. That pruning is
    /// how a closed socket is noticed — there is no separate reaper.
    pub fn broadcast(&mut self, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|_, tx| tx.send(msg.clone()).is_ok());
    }

    pub fn broadcast_except(&mut self, skip: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        self.subs.retain(|id, tx| id == skip || tx.send(msg.clone()).is_ok());
    }

    pub fn send_to(&mut self, id: &ConnId, ev: &Event) {
        let msg = crate::proto::encode(ev);
        if let Some(tx) = self.subs.get(id) {
            if tx.send(msg).is_err() {
                self.subs.remove(id);
            }
        }
    }

    pub fn snapshot_event(&self, origin: &ConnId) -> Event {
        Event::State { version: self.ws.version, origin: origin.clone(), ws: self.ws.view() }
    }

    fn persist(&mut self) {
        if let Err(e) = crate::wsstate::save(&self.project.clone(), &self.ws) {
            eprintln!("deadlight: state save failed: {e}");
        }
    }

    pub fn handle(&mut self, from: &ConnId, intent: Intent) {
        match &intent {
            Intent::RequestState => {
                let ev = self.snapshot_event(from);
                self.send_to(from, &ev);
                return;
            }
            Intent::EditBuffer { rel, text } => {
                // Text goes to everyone *but* the author, so their cursor survives.
                let ev = Event::BufferText {
                    rel: rel.clone(),
                    text: text.clone(),
                    origin: from.clone(),
                };
                if let Err(e) = workspace::apply_layout(&mut self.ws, &intent) {
                    let ev = Event::Error { msg: e };
                    self.send_to(from, &ev);
                    return;
                }
                self.ws.version += 1;
                self.broadcast_except(from, &ev);
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
                return;
            }
            Intent::SaveBuffer { rel, force } => return self.do_save(from, rel.clone(), *force),
            Intent::CreateFile { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::create_file(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::CreateDir { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::create_dir(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::DeleteFile { rel } => {
                let dir = self.dir.clone();
                let r = crate::fileops::delete(&dir, rel);
                return self.do_fileop(from, r);
            }
            Intent::RenamePath { from: f, to } => {
                let dir = self.dir.clone();
                let r = crate::fileops::rename(&dir, f, to);
                return self.do_fileop(from, r);
            }
            _ => {}
        }
        match workspace::apply_layout(&mut self.ws, &intent) {
            Ok(true) => {
                self.ws.version += 1;
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(false) => {}
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    /// A file with an open buffer changed on disk. Clean buffers follow the
    /// file so you watch Claude's edits land live; dirty buffers are only
    /// flagged stale, so unsaved work is never overwritten by a background
    /// writer.
    pub fn file_changed_externally(&mut self, base: &std::path::Path, rel: &str) {
        let Ok(disk) = std::fs::read_to_string(base.join(rel)) else { return };
        let disk_hash = workspace::hash_text(&disk);
        if crate::watch::is_self_write(&mut self.self_writes, rel, disk_hash) {
            return; // our own save; broadcasting it would echo back at the author
        }
        let Some(b) = self.ws.buffers.get_mut(rel) else { return };
        if b.dirty {
            b.stale = true;
            let ev = Event::BufferStale { rel: rel.to_string() };
            self.broadcast(&ev);
        } else {
            b.text = disk.clone();
            b.base_hash = disk_hash;
            b.stale = false;
            let ev = Event::BufferText {
                rel: rel.to_string(),
                text: disk,
                origin: String::new(), // no author: everyone applies it
            };
            self.broadcast(&ev);
        }
        self.ws.version += 1;
        self.broadcast(&Event::FileChanged { rel: rel.to_string() });
    }

    fn do_fileop(&mut self, from: &ConnId, r: Result<std::path::PathBuf, String>) {
        match r {
            Ok(_) => self.broadcast(&Event::TreeChanged),
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }

    fn do_save(&mut self, from: &ConnId, rel: String, force: bool) {
        let Some(buf) = self.ws.buffers.get(&rel).cloned() else {
            let ev = Event::Error { msg: format!("no buffer for {rel}") };
            return self.send_to(from, &ev);
        };
        let dir = self.dir.clone();
        match crate::fileops::save(&dir, &rel, &buf.text, buf.base_hash, force) {
            Ok(crate::fileops::SaveOutcome::Written) => {
                let hash = workspace::hash_text(&buf.text);
                if let Some(b) = self.ws.buffers.get_mut(&rel) {
                    b.dirty = false;
                    b.stale = false;
                    b.base_hash = hash;
                    b.base_mtime = std::fs::metadata(dir.join(&rel)).ok().and_then(|m| m.modified().ok());
                }
                self.self_writes.insert(rel.clone(), hash);
                self.ws.version += 1;
                self.broadcast(&Event::SaveOk { rel: rel.clone() });
                self.broadcast(&Event::FileChanged { rel });
                let snap = self.snapshot_event(from);
                self.broadcast(&snap);
                self.persist();
            }
            Ok(crate::fileops::SaveOutcome::Conflict { disk_text }) => {
                let diff_html = crate::render::diff_html(&conflict_diff(&disk_text, &buf.text));
                let ev = Event::SaveConflict { rel, diff_html };
                self.send_to(from, &ev);
            }
            Err(e) => {
                let ev = Event::Error { msg: e };
                self.send_to(from, &ev);
            }
        }
    }
}

/// A minimal unified-diff rendering of disk vs buffer. Uses the existing
/// classifier in `render`, so the conflict view looks like every other diff.
fn conflict_diff(disk: &str, buf: &str) -> String {
    let mut out = String::from("--- a/disk\n+++ b/your buffer\n@@ conflict @@\n");
    for l in disk.lines() {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in buf.lines() {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{self, Mode, Tab};

    // Helper: drain whatever a receiver has without blocking.
    fn drain(rx: &Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(m) = rx.try_recv() {
            out.push(m);
        }
        out
    }

    #[test]
    fn a_mutation_reaches_every_subscriber() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (_a, rx_a) = h.subscribe();
        let (b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&b, Intent::OpenTab {
            pane: proto::MIDDLE,
            tab: Tab::File { rel: "a.txt".into(), mode: Mode::Preview },
        });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(to_a.iter().any(|m| m.contains(r#""t":"State""#)), "the other client must mirror");
        assert!(to_b.iter().any(|m| m.contains(r#""t":"State""#)), "originator sees it too");
        assert!(to_a.iter().any(|m| m.contains("a.txt")));
    }

    #[test]
    fn buffer_text_is_not_echoed_to_its_author() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drain(&rx_a);
        drain(&rx_b);

        h.handle(&a, Intent::EditBuffer { rel: "a.txt".into(), text: "typed".into() });

        let to_a = drain(&rx_a);
        let to_b = drain(&rx_b);
        assert!(
            !to_a.iter().any(|m| m.contains(r#""t":"BufferText""#)),
            "echoing text back stomps the author's cursor"
        );
        assert!(to_b.iter().any(|m| m.contains("typed")), "other clients must receive the text");
        // Guard against `broadcast_except`'s retain predicate being inverted:
        // that bug would also make `to_a` empty (by pruning `a` outright)
        // and would otherwise pass the assertions above undetected.
        assert!(
            to_a.iter().any(|m| m.contains(r#""t":"State""#)),
            "author must survive broadcast_except"
        );
        assert_eq!(h.subs.len(), 2, "skipping the originator must not prune it");
    }

    #[test]
    fn version_advances_on_change_only() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        drain(&rx);
        drain(&rx_other);
        let before = h.ws.version;
        h.handle(&c, Intent::ActivateTab { pane: proto::MIDDLE, idx: 9 }); // invalid
        assert_eq!(h.ws.version, before, "a rejected intent must not bump the version");
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"Error""#)));
        // An Error is the requesting client's business, not a broadcast: a
        // second subscriber must see nothing, or `send_to` could silently
        // regress into `broadcast` without any test catching it.
        assert!(
            !drain(&rx_other).iter().any(|m| m.contains(r#""t":"Error""#)),
            "an Error must go only to the client that sent the bad intent"
        );
    }

    #[test]
    fn save_conflict_is_reported_and_the_file_is_untouched() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        std::fs::write(d.path().join("a.txt"), "on disk\n").unwrap();
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (c, rx) = h.subscribe();
        let (_other, rx_other) = h.subscribe();
        // buffer opened against different content => base_hash mismatch
        h.handle(&c, Intent::EditBuffer { rel: "a.txt".into(), text: "mine\n".into() });
        drain(&rx);
        drain(&rx_other);
        h.handle(&c, Intent::SaveBuffer { rel: "a.txt".into(), force: false });
        assert!(drain(&rx).iter().any(|m| m.contains(r#""t":"SaveConflict""#)));
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "on disk\n");
        // A conflict is the saving client's business, not everyone's: with
        // only one subscriber, `send_to` and `broadcast` are indistinguishable.
        assert!(
            !drain(&rx_other).iter().any(|m| m.contains(r#""t":"SaveConflict""#)),
            "a save conflict must go only to the client that tried to save"
        );
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let _g = crate::wsstate::STATE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("DEADLIGHT_STATE_DIR", d.path().join("state"));
        let mut h = Hub::new("proj", d.path().to_path_buf());
        let (a, rx_a) = h.subscribe();
        let (_b, rx_b) = h.subscribe();
        drop(rx_b);
        h.handle(&a, Intent::Resize { sizes: proto::Sizes::default() });
        assert_eq!(h.subs.len(), 1, "a closed socket must not accumulate");
        drop(rx_a);
    }
}
