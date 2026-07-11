// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

/// Fast, dev-facing pre-commit combo: gen → lint → check refs → heal drift
/// scan → owasp audit. Unlike `ci-check`, `ready` is the quick local loop —
/// it skips the external-tool gates (cargo audit, clippy, license/NOTICE) and
/// focuses on aden's own correctness plus documentation drift. Each step prints
/// a clear PASS/FAIL line; the run fails fast on the first hard error but always
/// emits a final commit-readiness summary. Returns Err if any hard gate failed.
pub fn cmd_ready(path: &Path, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let green = "\x1b[0;32m";
    let red = "\x1b[0;31m";
    let yellow = "\x1b[1;33m";
    let reset = "\x1b[0m";

    // (step label, passed?) — recorded for the final summary.
    let mut results: Vec<(&str, bool)> = Vec::new();
    // Hard failure: blocks all subsequent steps (fail-fast).
    let mut hard_failure: Option<String> = None;
    // Soft failure: recorded for the final verdict but does NOT block later steps.
    let mut soft_failure: Option<String> = None;
    let mut advisory_findings = 0usize;

    // Helper: run a hard gate unless we have already failed fast.
    macro_rules! step {
        ($name:expr, $body:expr) => {{
            if hard_failure.is_some() {
                println!(
                    "{}[ready] SKIP: {} (earlier step failed){}",
                    yellow, $name, reset
                );
                results.push(($name, false));
            } else {
                println!("[ready] Running: {} ...", $name);
                match $body {
                    Ok(()) => {
                        println!("{}[ready] PASS: {}{}", green, $name, reset);
                        results.push(($name, true));
                    }
                    Err(e) => {
                        let e: Box<dyn std::error::Error> = e;
                        println!("{}[ready] FAIL: {} — {}{}", red, $name, e, reset);
                        results.push(($name, false));
                        hard_failure = Some(format!("{}: {}", $name, e));
                    }
                }
            }
        }};
    }
    // Soft step: reports failure and continues — does NOT block subsequent steps.
    // Used for heal drift, which is a doc-quality signal, not a code-safety gate.
    macro_rules! soft_step {
        ($name:expr, $body:expr) => {{
            println!("[ready] Running: {} ...", $name);
            match $body {
                Ok(()) => {
                    println!("{}[ready] PASS: {}{}", green, $name, reset);
                    results.push(($name, true));
                }
                Err(e) => {
                    let e: Box<dyn std::error::Error> = e;
                    println!("{}[ready] WARN: {} — {}{}", yellow, $name, e, reset);
                    results.push(($name, false));
                    // Record as a soft failure so the final verdict still fails,
                    // but don't set hard_failure — let remaining steps (e.g. audit) run.
                    if soft_failure.is_none() {
                        soft_failure = Some(format!("{}: {}", $name, e));
                    }
                }
            }
        }};
    }

    println!("aden ready — pre-commit checks for {}\n", path.display());

    // (1) gen — recompile the project into the knowledge graph.
    step!("gen", { crate::commands::cmd_gen(path, true) });

    // (2) lint — fast line-based heuristics. --fix forwards to the linter.
    step!("lint", {
        crate::commands::cmd_lint(path, "Error", fix, false, false, false, false)
    });

    // (3) check refs — validate every <<ref>> resolves to an [[anchor]].
    step!("check refs", {
        if !path.is_dir() {
            Err("not a directory".into())
        } else {
            crate::util::perform_check(path).map(|_| ())
        }
    });

    // (4) heal drift scan — doc-mismatch gate. Drift (broken refs, orphan
    // anchors, signature mismatch, or a degraded health score) is a reportable
    // hard signal here, per the pre-commit intent: never commit stale docs.
    // With --fix we apply high-confidence auto-fixes first, then re-scan.
    soft_step!("heal drift scan", {
        use aden_heal::{Scanner, generate};
        if fix {
            // Auto-apply StaleHash/MissingContract fixes before judging drift.
            let _ = crate::commands::cmd_heal_scan(path, false, true, false, false, false, None);
        }
        let scanner = Scanner::new(path);
        let events = scanner.scan()?;
        // Only hard correctness failures matter to this gate:
        // - BrokenReference (Critical): a <<ref>> points at a missing anchor
        // - SignatureMismatch (High): a symbol's signature changed
        // OrphanAnchor (Medium) and MissingContract (Medium) are maintenance
        // signals — stale store entries and undocumented/metadata doc nodes —
        // not pre-commit blockers. Run `aden sync` to clean them up. The same
        // classification that `diagnose`/`status` use to report 100/100 while
        // hundreds of metadata doc nodes lack contracts; the score gate below
        // MUST agree, so it judges the score over these hard events only rather
        // than letting doc-node MissingContracts crater it to 0.00.
        let is_hard = |e: &aden_heal::DriftEvent| {
            matches!(
                e,
                aden_heal::DriftEvent::BrokenReference { .. }
                    | aden_heal::DriftEvent::SignatureMismatch { .. }
                    | aden_heal::DriftEvent::DocSignatureDivergence { .. }
            )
        };
        let hard_events: Vec<_> = events.iter().filter(|e| is_hard(e)).cloned().collect();
        advisory_findings += events.len().saturating_sub(hard_events.len());
        let report = generate(hard_events, path);
        let critical = events.iter().filter(|e| is_hard(e)).count();
        if critical > 0 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "{} critical drift event(s) (broken refs, signature mismatch, doc/code divergence) — run 'aden heal' to inspect",
                critical
            )))
        } else if report.overall_score < 0.90 {
            Err(Box::<dyn std::error::Error>::from(format!(
                "doc drift: health score {:.2} (< 0.90) — run 'aden gen' / 'aden heal . --fix'",
                report.overall_score
            )))
        } else {
            Ok(())
        }
    });

    // (5) audit — OWASP-aligned source scan (in-process, no external tools).
    step!("owasp audit", {
        crate::commands::cmd_audit(path, None, "text", true, false)
    });

    // ── Final verdict ─────────────────────────────────────
    println!("\n[ready] Summary:");
    for (name, passed) in &results {
        let (mark, color) = if *passed {
            ("PASS", green)
        } else {
            ("FAIL", red)
        };
        println!("  {}{:>4}{} {}", color, mark, reset, name);
    }

    let any_failure = hard_failure.or(soft_failure);
    if let Some(reason) = any_failure {
        println!("[ready] Outcome: blocked (graph/policy/freshness reported independently above)");
        println!(
            "\n{}[ready] NOT commit-ready — fix the failing step: {}{}",
            red, reason, reset
        );
        return Err(reason.into());
    }

    if advisory_findings > 0 {
        println!(
            "\n{}[ready] Blocking checks passed with {} advisory finding(s).{}",
            yellow, advisory_findings, reset
        );
        println!("[ready] Outcome: passed_with_findings");
    } else {
        println!(
            "\n{}[ready] All checks passed — tree looks commit-ready.{}",
            green, reset
        );
        println!("[ready] Outcome: clean");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_ready_runs_on_temp_project() {
        let dir = std::env::temp_dir().join(format!("aden-ready-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A trivial Rust project: the pipeline has real source to run against.
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn noop() {}\n").unwrap();

        // Must complete without panicking and yield a verdict (Ok or Err).
        let _verdict = cmd_ready(&dir, false);
        let _ = std::fs::remove_dir_all(&dir);

        // A non-directory path is a hard failure at the "check refs" gate, so
        // ready must report NOT commit-ready (Err).
        let missing = dir.join("does-not-exist");
        assert!(
            cmd_ready(&missing, false).is_err(),
            "ready should fail when the target path is not a directory"
        );
    }
}
