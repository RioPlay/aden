// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::drift::DriftEvent;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct HealthReport {
    pub repo_path: String,
    pub overall_score: f64,
    pub events: Vec<DriftEvent>,
}

pub fn generate(events: Vec<DriftEvent>, repo_path: &Path) -> HealthReport {
    let mut total_contracts = 0usize;
    let _ = count_contracts(repo_path, &mut total_contracts);

    let weighted_count: f64 = events.iter().map(|e| e.severity().weight()).sum();

    let score = if total_contracts > 0 {
        let s = 1.0 - (weighted_count / total_contracts as f64);
        s.max(0.0)
    } else {
        1.0
    };

    HealthReport {
        repo_path: repo_path.to_string_lossy().to_string(),
        overall_score: score,
        events,
    }
}

fn count_contracts(dir: &Path, count: &mut usize) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            count_contracts(&path, count)?;
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "adoc" || ext == "aden" {
                *count += 1;
            }
        }
    }
    Ok(())
}
