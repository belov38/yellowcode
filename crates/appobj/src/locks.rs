use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockControl {
    Automatic,
    Managed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OwnerId(u64);

#[derive(Debug, PartialEq, Eq)]
pub struct Conflict;

struct Hold {
    owner: OwnerId,
    thread: ThreadId,
    mode: LockMode,
}

#[derive(Default)]
struct State {
    spaces: HashMap<String, Vec<Hold>>,
    by_owner: HashMap<OwnerId, Vec<String>>,
    waiting: usize,
}

enum Meets {
    Nobody,
    ThisThread,
    OtherThreads,
}

impl State {
    fn meets(&self, owner: OwnerId, thread: ThreadId, key: &str, mode: LockMode) -> Meets {
        let mut found = Meets::Nobody;
        for hold in self.spaces.get(key).into_iter().flatten() {
            if hold.owner == owner || (mode == LockMode::Shared && hold.mode == LockMode::Shared) {
                continue;
            }
            if hold.thread == thread {
                return Meets::ThisThread;
            }
            found = Meets::OtherThreads;
        }
        found
    }

    fn grant(&mut self, owner: OwnerId, thread: ThreadId, key: &str, mode: LockMode) {
        let holds = self.spaces.entry(key.to_owned()).or_default();
        if let Some(own) = holds.iter_mut().find(|h| h.owner == owner) {
            if mode == LockMode::Exclusive {
                own.mode = LockMode::Exclusive;
            }
            return;
        }
        holds.push(Hold {
            owner,
            thread,
            mode,
        });
        self.by_owner.entry(owner).or_default().push(key.to_owned());
    }
}

#[derive(Default)]
pub struct LockTable {
    state: Mutex<State>,
    changed: Condvar,
    next_owner: AtomicU64,
}

impl LockTable {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn new_owner(self: &Arc<Self>) -> Held {
        let id = self.next_owner.fetch_add(1, Ordering::Relaxed);
        Held {
            table: self.clone(),
            owner: OwnerId(id),
        }
    }

    pub fn acquire(
        &self,
        owner: OwnerId,
        key: &str,
        mode: LockMode,
        wait: Duration,
    ) -> Result<(), Conflict> {
        let thread = std::thread::current().id();
        let deadline = Instant::now().checked_add(wait);
        let mut state = self.state();
        loop {
            match state.meets(owner, thread, key, mode) {
                Meets::Nobody => {
                    state.grant(owner, thread, key, mode);
                    return Ok(());
                }
                Meets::ThisThread => return Err(Conflict),
                Meets::OtherThreads => {}
            }
            let left = deadline.map_or(wait, |d| d.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return Err(Conflict);
            }
            state.waiting += 1;
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            state.waiting = state.waiting.saturating_sub(1);
        }
    }

    pub fn release_all(&self, owner: OwnerId) {
        let mut state = self.state();
        let Some(keys) = state.by_owner.remove(&owner) else {
            return;
        };
        for key in keys {
            if let Some(holds) = state.spaces.get_mut(&key) {
                holds.retain(|h| h.owner != owner);
                if holds.is_empty() {
                    state.spaces.remove(&key);
                }
            }
        }
        drop(state);
        self.changed.notify_all();
    }

    pub fn holders(&self, key: &str) -> Vec<(OwnerId, LockMode)> {
        self.state()
            .spaces
            .get(key)
            .into_iter()
            .flatten()
            .map(|h| (h.owner, h.mode))
            .collect()
    }

    pub fn spaces_held(&self) -> usize {
        self.state().spaces.len()
    }

    pub fn holds_any(&self, owner: OwnerId) -> bool {
        self.state().by_owner.contains_key(&owner)
    }

    pub fn waiting(&self) -> usize {
        self.state().waiting
    }
}

pub struct Held {
    table: Arc<LockTable>,
    owner: OwnerId,
}

impl Held {
    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn holds_any(&self) -> bool {
        self.table.holds_any(self.owner)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.table.release_all(self.owner);
    }
}
