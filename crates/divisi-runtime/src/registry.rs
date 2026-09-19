//! In-memory, daemon-lifetime registry of currently-running background
//! tasks — the first piece of mutable state that outlives a single
//! request (everything else, per `Context::load`'s doc comment, is
//! rebuilt fresh per request). Exists solely to let `TaskCancel{id}` find
//! and flip a running task's cancel flag; created once in
//! `server::serve` and cloned (cheaply — it's an `Arc` underneath) into
//! every connection handler, so all of them share the same map rather
//! than each getting its own.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct TaskRegistry(Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>);

impl TaskRegistry {
    /// Creates and stores a fresh, unset cancel flag for `id`, returning
    /// the same `Arc` so the caller can hand a clone to the background
    /// thread that will poll it.
    pub fn register(&self, id: i64) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.0.lock().unwrap_or_else(|p| p.into_inner()).insert(id, Arc::clone(&flag));
        flag
    }

    /// Flips `id`'s cancel flag if it's currently registered (i.e. the
    /// task is actually running in the background). Returns `false` for a
    /// task that already finished or was never started with
    /// `background: true` — the caller (a `TaskCancel` request) uses this
    /// to report an honest "nothing to cancel" rather than claiming
    /// success it didn't actually accomplish.
    pub fn cancel(&self, id: i64) -> bool {
        match self.0.lock().unwrap_or_else(|p| p.into_inner()).get(&id) {
            Some(flag) => {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Removes `id`'s entry once its background thread is done with it —
    /// called unconditionally (success or failure) so the map doesn't
    /// grow forever across a long-lived daemon's lifetime.
    pub fn unregister(&self, id: i64) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_on_a_registered_task_flips_its_flag_and_reports_true() {
        let registry = TaskRegistry::default();
        let flag = registry.register(1);
        assert!(!flag.load(std::sync::atomic::Ordering::Relaxed));
        assert!(registry.cancel(1));
        assert!(flag.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn cancel_on_an_unknown_task_reports_false() {
        let registry = TaskRegistry::default();
        assert!(!registry.cancel(999));
    }

    #[test]
    fn unregister_makes_a_later_cancel_report_false() {
        let registry = TaskRegistry::default();
        registry.register(1);
        registry.unregister(1);
        assert!(!registry.cancel(1));
    }

    #[test]
    fn clones_share_the_same_underlying_map() {
        let registry = TaskRegistry::default();
        let clone = registry.clone();
        clone.register(1);
        assert!(registry.cancel(1), "a clone's registration must be visible through the original handle");
    }
}
