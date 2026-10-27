use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
static DENY: AtomicBool = AtomicBool::new(false);
static FALLBACKS: AtomicUsize = AtomicUsize::new(0);
pub fn deny_spawns() {
    DENY.store(true, Ordering::Release);
}
pub(crate) fn denied() -> bool {
    DENY.load(Ordering::Acquire)
}
pub(crate) fn did_fallback() {
    FALLBACKS.fetch_add(1, Ordering::Relaxed);
}
pub fn fallback_count() -> usize {
    FALLBACKS.load(Ordering::Relaxed)
}
