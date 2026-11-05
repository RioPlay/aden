// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Opt-in generation diagnostics. Only an explicit successful finish emits a
//! report: errors and early returns (no files, coalesced refresh, dry-run) omit
//! it, so machine error envelopes never acquire a trailing diagnostic.
//!
//! Stage intervals are wall time, mutually exclusive, and accumulate when a
//! stage is resumed. `total_us` includes all stages (and therefore overlaps
//! them); `parse_prepare` measures the whole parallel pass, not worker CPU sums.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Instant;

#[derive(Default, serde::Serialize)]
struct Stage {
    elapsed_us: u64,
    intervals: u64,
}

struct EnabledTrace {
    started: Instant,
    stage_started: Instant,
    stage: &'static str,
    stages: BTreeMap<&'static str, Stage>,
    counts: BTreeMap<&'static str, usize>,
    quiet: bool,
    silent: bool,
}

pub(super) struct GenTrace(Option<EnabledTrace>);

impl GenTrace {
    pub(super) fn new(quiet: bool, silent: bool) -> Self {
        Self(
            (std::env::var_os("ADEN_GEN_TRACE").as_deref() == Some(std::ffi::OsStr::new("1")))
                .then(|| {
                    let started = Instant::now();
                    EnabledTrace {
                        started,
                        stage_started: started,
                        stage: "root_resolution",
                        stages: BTreeMap::new(),
                        counts: BTreeMap::new(),
                        quiet,
                        silent,
                    }
                }),
        )
    }

    pub(super) fn stage(&mut self, stage: &'static str) {
        if let Some(trace) = self.0.as_mut() {
            let now = Instant::now();
            trace.close_stage(now);
            trace.stage_started = now;
            trace.stage = stage;
        }
    }

    pub(super) fn count(&mut self, name: &'static str, count: usize) {
        if let Some(trace) = self.0.as_mut() {
            trace.counts.insert(name, count);
        }
    }

    pub(super) fn increment(&mut self, name: &'static str) {
        if let Some(trace) = self.0.as_mut() {
            *trace.counts.entry(name).or_default() += 1;
        }
    }

    pub(super) fn finish(self) {
        let Some(mut trace) = self.0 else { return };
        let now = Instant::now();
        trace.close_stage(now);
        let report = serde_json::json!({
            "type": "aden_gen_trace",
            "schema_version": 1,
            "outcome": "completed",
            "quiet": trace.quiet,
            "silent": trace.silent,
            "timing": "exclusive_stages_inclusive_total_wall_time",
            "total_us": micros(now.duration_since(trace.started)),
            "stages": trace.stages,
            "counts": trace.counts,
        });
        // Best-effort diagnostics must never change the command's result or
        // panic when stderr is closed. One compact JSON object per line.
        let _ = writeln!(std::io::stderr().lock(), "{report}");
    }
}

impl EnabledTrace {
    fn close_stage(&mut self, now: Instant) {
        let entry = self.stages.entry(self.stage).or_default();
        entry.elapsed_us = entry
            .elapsed_us
            .saturating_add(micros(now.duration_since(self.stage_started)));
        entry.intervals += 1;
    }
}

fn micros(duration: std::time::Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}
