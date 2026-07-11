// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

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
