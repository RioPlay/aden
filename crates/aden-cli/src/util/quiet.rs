use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Set the global quiet mode. Should be called once during startup.
pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

/// Query whether we are in quiet mode.
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}
