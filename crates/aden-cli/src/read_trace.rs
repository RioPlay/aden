// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Opt-in local read diagnostics. No source, query, path, or extra filesystem work.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STATS: Mutex<Stats> = Mutex::new(Stats::new());

#[derive(Clone, Copy)]
pub enum Stage {
    Fingerprint,
    Refresh,
    EnsureFresh,
    IndexLoad,
    IndexBuild,
    RootResolve,
    StoreResolve,
    StoreCompatibility,
    ManifestLoad,
}

#[derive(Clone, Copy, Default, serde::Serialize)]
struct StageTotal {
    calls: u64,
    elapsed_us: u64,
}

struct Stats {
    stages: [StageTotal; 9],
    fingerprint_files: u64,
    fingerprint_bytes: u64,
    index_cache_hits: u64,
    index_cache_misses: u64,
    compatibility_store_opens: u64,
}

impl Stats {
    const fn new() -> Self {
        Self {
            stages: [StageTotal {
                calls: 0,
                elapsed_us: 0,
            }; 9],
            fingerprint_files: 0,
            fingerprint_bytes: 0,
            index_cache_hits: 0,
            index_cache_misses: 0,
            compatibility_store_opens: 0,
        }
    }
}

pub struct Request {
    started: Option<Instant>,
}

impl Request {
    /// Called at real_main entry so the total includes CLI argument parsing.
    pub fn start() -> Self {
        Self {
            started: (std::env::var("ADEN_READ_TRACE").as_deref() == Ok("1")).then(Instant::now),
        }
    }

    pub fn select_command(&self, command: Option<&str>) {
        ACTIVE.store(
            self.started.is_some() && is_read(command),
            Ordering::Relaxed,
        );
    }

    /// Only successful read commands emit. Errors keep their existing stderr
    /// protocol intact, including the single JSON machine-error envelope.
    pub fn success(self, command: Option<&str>) {
        let Some(started) = self.started else { return };
        if !is_read(command) {
            return;
        }
        ACTIVE.store(false, Ordering::Relaxed);
        let Ok(stats) = STATS.lock() else { return };
        let report = serde_json::json!({
            "schema_version": 1,
            "type": "aden_read_trace",
            "command": command,
            "total_elapsed_us": micros(started),
            "timing_semantics": "inclusive; stages may overlap and must not be summed",
            "stages": {
                "fingerprint": stats.stages[Stage::Fingerprint as usize],
                "refresh": stats.stages[Stage::Refresh as usize],
                "ensure_fresh": stats.stages[Stage::EnsureFresh as usize],
                "index_load": stats.stages[Stage::IndexLoad as usize],
                "index_build": stats.stages[Stage::IndexBuild as usize],
                "root_resolve": stats.stages[Stage::RootResolve as usize],
                "store_resolve": stats.stages[Stage::StoreResolve as usize],
                "store_compatibility": stats.stages[Stage::StoreCompatibility as usize],
                "manifest_load": stats.stages[Stage::ManifestLoad as usize],
            },
            "counters": {
                "fingerprint_scans": stats.stages[Stage::Fingerprint as usize].calls,
                "fingerprint_files": stats.fingerprint_files,
                "fingerprint_bytes": stats.fingerprint_bytes,
                "index_cache_hits": stats.index_cache_hits,
                "index_cache_misses": stats.index_cache_misses,
                "compatibility_store_opens": stats.compatibility_store_opens,
            },
        });
        // Diagnostics must not turn a successful read into a failure when a
        // terminal/log consumer closes stderr early.
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "{report}");
    }
}

fn is_read(command: Option<&str>) -> bool {
    matches!(
        command,
        Some(
            "read" | "grep" | "locate" | "understand" | "asm" | "query" | "search" | "tree" | "ask"
        )
    )
}

fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}

pub struct Span {
    stage: Stage,
    started: Option<Instant>,
    files: u64,
    bytes: u64,
}

pub fn span(stage: Stage) -> Span {
    Span {
        stage,
        started: ACTIVE.load(Ordering::Relaxed).then(Instant::now),
        files: 0,
        bytes: 0,
    }
}

impl Span {
    pub fn scanned_file(&mut self) {
        if self.started.is_some() {
            self.files = self.files.saturating_add(1);
        }
    }

    pub fn scanned_bytes(&mut self, bytes: usize) {
        if self.started.is_some() {
            self.bytes = self.bytes.saturating_add(bytes as u64);
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(started) = self.started else { return };
        if let Ok(mut stats) = STATS.lock() {
            let total = &mut stats.stages[self.stage as usize];
            total.calls = total.calls.saturating_add(1);
            total.elapsed_us = total.elapsed_us.saturating_add(micros(started));
            stats.fingerprint_files = stats.fingerprint_files.saturating_add(self.files);
            stats.fingerprint_bytes = stats.fingerprint_bytes.saturating_add(self.bytes);
        }
    }
}

pub fn index_cache_result(hit: bool) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut stats) = STATS.lock() {
        if hit {
            stats.index_cache_hits = stats.index_cache_hits.saturating_add(1);
        } else {
            stats.index_cache_misses = stats.index_cache_misses.saturating_add(1);
        }
    }
}

pub fn compatibility_store_open() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut stats) = STATS.lock() {
        stats.compatibility_store_opens = stats.compatibility_store_opens.saturating_add(1);
    }
}
