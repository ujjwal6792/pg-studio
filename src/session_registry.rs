//! O(1) session lookup keyed by project name.
//!
//! `App` used to keep sessions in a `Vec` and linearly scan it (locking
//! every entry) on each lookup. The projects list calls `session_for` /
//! `is_project_running` once per visible row at 10Hz, so that scan is
//! O(S·V) per frame. This registry owns a `HashMap<String,
//! Arc<Mutex<RunningSession>>>` keyed by project name plus an insertion
//! order for the iteration sites (`shutdown`, `poll_auto_open`), so lookups
//! take no locks to find the entry and at most one lock to inspect it.
//!
//! The registry deliberately stays a dumb store: lifecycle (`stop`, detach,
//! tailing) lives in `App`. It also keeps a small `Vec`-compatible surface
//! (`push` / `len` / `is_empty` / `clear` / `&registry` iteration) so
//! existing call sites keep compiling unchanged.

use crate::session::{RunningSession, SessionStatus};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Session store keyed by project name, preserving insertion order.
#[derive(Default)]
pub struct SessionRegistry {
    map: HashMap<String, Arc<Mutex<RunningSession>>>,
    order: Vec<String>,
}

impl SessionRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `session` under `name`, replacing any previous entry.
    /// First-insertion order is preserved for iteration.
    pub fn insert(&mut self, name: String, session: Arc<Mutex<RunningSession>>) {
        if !self.map.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.map.insert(name, session);
    }

    /// `Vec`-compatible push: derives the key from the session itself.
    /// A poisoned mutex yields an empty-string key rather than panicking;
    /// the entry is still stored so iteration sites observe it.
    pub fn push(&mut self, session: Arc<Mutex<RunningSession>>) {
        let name = session
            .lock()
            .map(|s| s.project_name.clone())
            .unwrap_or_default();
        self.insert(name, session);
    }

    /// O(1) lookup by project name. Never locks to find the entry.
    pub fn get(&self, name: &str) -> Option<Arc<Mutex<RunningSession>>> {
        self.map.get(name).cloned()
    }

    /// True when `name` has a live (starting/pulling/running) session.
    /// Takes at most one lock; a poisoned mutex counts as not running.
    pub fn is_running(&self, name: &str) -> bool {
        self.map
            .get(name)
            .map(|s| {
                s.lock()
                    .map(|g| {
                        g.project_name == name
                            && matches!(
                                g.status,
                                SessionStatus::Starting
                                    | SessionStatus::Pulling
                                    | SessionStatus::Running
                            )
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Remove the entry for `name`, returning it when present.
    pub fn remove(&mut self, name: &str) -> Option<Arc<Mutex<RunningSession>>> {
        let removed = self.map.remove(name);
        if removed.is_some() {
            self.order.retain(|n| n != name);
        }
        removed
    }

    /// Number of stored sessions.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True when no sessions are stored.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Drop every entry.
    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    /// Entries in insertion order. Stale order keys (no map entry) are
    /// skipped so a missed `remove` can never surface a phantom session.
    pub fn iter(&self) -> SessionIter<'_> {
        SessionIter {
            registry: self,
            pos: 0,
        }
    }
}

/// Insertion-order iterator over [`SessionRegistry`] entries.
pub struct SessionIter<'a> {
    registry: &'a SessionRegistry,
    pos: usize,
}

impl<'a> Iterator for SessionIter<'a> {
    type Item = &'a Arc<Mutex<RunningSession>>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(name) = self.registry.order.get(self.pos) {
            self.pos += 1;
            if let Some(session) = self.registry.map.get(name) {
                return Some(session);
            }
        }
        None
    }
}

impl<'a> IntoIterator for &'a SessionRegistry {
    type Item = &'a Arc<Mutex<RunningSession>>;
    type IntoIter = SessionIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_named(name: &str, status: SessionStatus) -> Arc<Mutex<RunningSession>> {
        Arc::new(Mutex::new(RunningSession {
            project_name: name.to_string(),
            studio_port: 1,
            ssh: None,
            studio_child: None,
            status,
            logs: Arc::new(Mutex::new(Vec::new())),
            tunnel_url: None,
            error: None,
            auto_open: false,
            studio_ready: false,
            studio_pid: None,
            ssh_pid: None,
            log_path: None,
            started_at: None,
        }))
    }

    fn poison(session: &Arc<Mutex<RunningSession>>) {
        let victim = Arc::clone(session);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = victim.lock().unwrap();
            panic!("intentional poisoning for test");
        }));
        assert!(session.is_poisoned());
    }

    #[test]
    fn get_returns_entry_without_scanning_others() {
        let mut reg = SessionRegistry::new();
        reg.insert("a".to_string(), session_named("a", SessionStatus::Running));
        reg.insert("b".to_string(), session_named("b", SessionStatus::Stopped));
        assert!(reg.get("missing").is_none());
        let b = reg.get("b").unwrap();
        assert_eq!(b.lock().unwrap().project_name, "b");
        assert_eq!(reg.len(), 2);
        assert!(!reg.is_empty());
    }

    #[test]
    fn insert_replaces_without_duplicating_order() {
        let mut reg = SessionRegistry::new();
        reg.push(session_named("a", SessionStatus::Starting));
        reg.push(session_named("b", SessionStatus::Running));
        reg.insert("a".to_string(), session_named("a", SessionStatus::Error));
        let names: Vec<String> = reg
            .iter()
            .map(|s| s.lock().unwrap().project_name.clone())
            .collect();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(reg.len(), 2);
        assert_eq!(
            reg.get("a").unwrap().lock().unwrap().status,
            SessionStatus::Error
        );
    }

    #[test]
    fn is_running_matches_only_live_statuses() {
        let mut reg = SessionRegistry::new();
        reg.push(session_named("run", SessionStatus::Running));
        reg.push(session_named("start", SessionStatus::Starting));
        reg.push(session_named("pull", SessionStatus::Pulling));
        reg.push(session_named("err", SessionStatus::Error));
        reg.push(session_named("stop", SessionStatus::Stopped));
        assert!(reg.is_running("run"));
        assert!(reg.is_running("start"));
        assert!(reg.is_running("pull"));
        assert!(!reg.is_running("err"));
        assert!(!reg.is_running("stop"));
        assert!(!reg.is_running("missing"));
    }

    #[test]
    fn remove_and_clear_drop_entries_and_order() {
        let mut reg = SessionRegistry::new();
        reg.push(session_named("a", SessionStatus::Running));
        reg.push(session_named("b", SessionStatus::Running));
        assert!(reg.remove("missing").is_none());
        assert!(reg.remove("a").is_some());
        assert!(reg.get("a").is_none());
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.iter().count(), 1);
        reg.clear();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.iter().count(), 0);
    }

    #[test]
    fn poisoned_sessions_never_panic() {
        let mut reg = SessionRegistry::new();
        let bad = session_named("bad", SessionStatus::Running);
        poison(&bad);
        reg.insert("bad".to_string(), bad);
        reg.push(session_named("good", SessionStatus::Running));

        // Key lookup needs no lock, so the entry is still found.
        assert!(reg.get("bad").is_some());
        // Status inspection tolerates the poisoned mutex: not running.
        assert!(!reg.is_running("bad"));
        assert!(reg.is_running("good"));

        // Vec-compatible iteration over a poisoned entry must not panic.
        let count = reg.iter().filter(|s| s.lock().is_ok()).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn ref_iteration_matches_insertion_order() {
        let mut reg = SessionRegistry::new();
        for name in ["one", "two", "three"] {
            reg.push(session_named(name, SessionStatus::Running));
        }
        let names: Vec<String> = (&reg)
            .into_iter()
            .map(|s| s.lock().unwrap().project_name.clone())
            .collect();
        assert_eq!(names, vec!["one", "two", "three"]);
    }
}
