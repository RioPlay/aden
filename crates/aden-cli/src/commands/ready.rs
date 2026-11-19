// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepStatus {
    Passed,
    Failed,
    Skipped,
}

impl StepStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

struct ReadyStep {
    name: &'static str,
    status: StepStatus,
    evidence_kind: &'static str,
    scope: &'static str,
    reason: String,
}

fn check_blocking_messages(messages: &[String]) -> Result<usize, Box<dyn std::error::Error>> {
    // Match the same Error/Warning classification as `aden check`.
    let (errors, warnings, _) = crate::util::classify_check_messages(messages);
    if errors.is_empty() {
        Ok(warnings.len())
    } else {
        Err(format!(
            "{} blocking check finding(s): {}",
            errors.len(),
            errors
                .iter()
                .take(3)
                .map(|error| format!("ERROR: {error}"))
                .collect::<Vec<_>>()
                .join("; ")
        )
        .into())
    }
}

/// Run Aden's local static checks and report each check's outcome and scope.
/// Generation refreshes the graph. Source repairs require explicit `--fix`.
/// A passing result does not establish runtime behavior, project test results,
/// or the external-tool gates provided by `ci-check`.
///
/// JSON is the CLI/MCP default. Child steps keep stdout quiet so this command
/// emits one JSON document. Hard failures skip remaining checks; drift failures
/// are recorded but allow the final audit to run.
pub fn cmd_ready(path: &Path, fix: bool, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !path.is_dir() {
        return Err(format!("Ready target is not a directory: {}", path.display()).into());
    }
    let (green, red, yellow, reset) = if json {
        ("", "", "", "")
    } else {
        ("\x1b[0;32m", "\x1b[0;31m", "\x1b[1;33m", "\x1b[0m")
    };
    let mut results: Vec<ReadyStep> = Vec::new();
    let mut hard_failure: Option<String> = None;
    let mut soft_failure: Option<String> = None;
    let mut advisory_findings = 0usize;
    let mut heal_repair_attempted = false;
    let mut repair_failure: Option<String> = None;
    let mut heal_repairs = None;

    macro_rules! step {
        ($name:expr, $kind:expr, $scope:expr, $soft:expr, $body:block) => {{
            if let Some(blocker) = hard_failure.as_deref() {
                let reason = format!("Not run because an earlier check failed: {blocker}");
                if !json {
                    println!("{}[ready] SKIP: {} - {}{}", yellow, $name, reason, reset);
                }
                results.push(ReadyStep {
                    name: $name,
                    status: StepStatus::Skipped,
                    evidence_kind: $kind,
                    scope: $scope,
                    reason,
                });
            } else {
                if !json {
                    println!("[ready] Running: {} ...", $name);
                }
                // Keep `?` inside the step so every failure reaches the summary.
                // The shared macro needs this boundary even for simple steps.
                #[allow(clippy::redundant_closure_call)]
                let result = (|| -> Result<(), Box<dyn std::error::Error>> { $body })();
                let (status, reason) = match result {
                    Ok(()) => {
                        if !json {
                            println!("{}[ready] PASS: {}{}", green, $name, reset);
                        }
                        (
                            StepStatus::Passed,
                            "No blocking finding reported by this check".to_string(),
                        )
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        if !json {
                            let mark = "FAIL";
                            println!("{}[ready] {}: {} - {}{}", red, mark, $name, reason, reset);
                        }
                        let failure = format!("{}: {}", $name, reason);
                        if $soft {
                            soft_failure.get_or_insert(failure);
                        } else {
                            hard_failure = Some(failure);
                        }
                        (StepStatus::Failed, reason)
                    }
                };
                results.push(ReadyStep {
                    name: $name,
                    status,
                    evidence_kind: $kind,
                    scope: $scope,
                    reason,
                });
            }
        }};
    }

    if !json {
        println!("aden ready - local static checks for {}\n", path.display());
        println!("[ready] Refreshes the graph; source repairs requested: {fix}");
    }

    step!(
        "gen",
        "index_generation",
        "Refresh the local knowledge graph",
        false,
        {
            // The request waits for readiness, but does not own the build. A
            // pending result skips dependent checks while the worker continues.
            crate::indexer::generation_job::wait_for_completion(
                path,
                std::time::Duration::from_secs(30),
            )
            .map(|_| ())
        }
    );

    step!(
        "lint",
        "heuristic_signal",
        "Aden source lint rules at Error severity",
        false,
        { crate::commands::cmd_lint(path, "Error", fix, false, false, false, json) }
    );

    step!(
        "check refs",
        "structural_invariant",
        "Aden reference, graph and contract checks",
        false,
        {
            let messages = crate::util::perform_check(path)?;
            advisory_findings += check_blocking_messages(&messages)?;
            Ok(())
        }
    );

    step!(
        "heal drift scan",
        "inferred_graph_check",
        "Broken references, signature mismatch and doc/code divergence",
        true,
        {
            use aden_heal::Scanner;
            let scanner = Scanner::new(path);
            let mut events = scanner.scan()?;
            if fix {
                heal_repair_attempted = true;
                let summary =
                    super::heal::repair_supported_drift(path, &events).inspect_err(|error| {
                        repair_failure = Some(error.to_string());
                    })?;
                let failed = summary.failed > 0 || summary.conflicts > 0;
                let reason = format!(
                    "repair failed: {} failure(s), {} conflict(s): {}",
                    summary.failed,
                    summary.conflicts,
                    summary.reasons.join("; ")
                );
                heal_repairs = Some(summary);
                if failed {
                    repair_failure = Some(reason.clone());
                    return Err(reason.into());
                }
                events = scanner.scan()?;
            }
            let blocking_events = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        aden_heal::DriftEvent::BrokenReference { .. }
                            | aden_heal::DriftEvent::SignatureMismatch { .. }
                            | aden_heal::DriftEvent::DocSignatureDivergence { .. }
                    )
                })
                .count();
            advisory_findings += events.len().saturating_sub(blocking_events);
            if blocking_events > 0 {
                Err(format!(
                "{blocking_events} blocking drift event(s) (broken refs, signature mismatch, doc/code divergence); run 'aden heal' to inspect"
            ).into())
            } else {
                Ok(())
            }
        }
    );

    step!(
        "owasp audit",
        "heuristic_signal",
        "Aden's in-process OWASP-aligned source rules",
        false,
        { crate::commands::audit::cmd_audit_with_output(path, None, "text", true, false, json) }
    );

    let any_failure = hard_failure.or(soft_failure);
    let (ok, outcome) = if any_failure.is_some() {
        (false, "blocked")
    } else if advisory_findings > 0 {
        (true, "passed_with_findings")
    } else {
        (true, "clean")
    };
    if json {
        let env = serde_json::json!({
            "schema_version": 1,
            "command": "ready",
            "ok": ok,
            "result_state": if ok { "complete" } else { "failed" },
            "outcome": outcome,
            "advisory_findings": advisory_findings,
            "hard_failure": any_failure.as_deref(),
            "assessment_scope": "local_static_checks",
            "graph_refresh_requested": true,
            "source_repairs_requested": fix,
            "heal_repair_attempted": heal_repair_attempted,
            "heal_repairs": heal_repairs,
            "repair_failure": repair_failure,
            "unverified": ["runtime_behavior", "project_tests", "external_tool_gates"],
            "steps": results.iter().map(|step| {
                serde_json::json!({
                    "name": step.name,
                    "passed": step.status == StepStatus::Passed,
                    "status": step.status.as_str(),
                    "evidence_kind": step.evidence_kind,
                    "scope": step.scope,
                    "reason": step.reason,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string(&env)?);
    } else {
        if let Some(summary) = heal_repairs.as_ref() {
            println!(
                "[ready] Repairs: {} fixed, {} skipped, {} failed, {} conflicts",
                summary.fixed, summary.skipped, summary.failed, summary.conflicts
            );
        }
        println!("\n[ready] Summary:");
        for step in &results {
            let (mark, color) = match step.status {
                StepStatus::Passed => ("PASS", green),
                StepStatus::Failed => ("FAIL", red),
                StepStatus::Skipped => ("SKIP", yellow),
            };
            println!(
                "  {}{:>4}{} {} ({}): {}",
                color, mark, reset, step.name, step.evidence_kind, step.scope
            );
        }
        println!("[ready] Not evaluated: runtime behavior, project tests, external-tool gates.");
        println!("[ready] Outcome: {outcome}");
        if let Some(reason) = any_failure.as_deref() {
            println!("\n{red}[ready] Local static checks blocked: {reason}{reset}");
        } else if advisory_findings > 0 {
            println!(
                "\n{yellow}[ready] Local blocking checks passed with {advisory_findings} advisory finding(s).{reset}"
            );
        } else {
            println!(
                "\n{green}[ready] Local static checks passed within the scopes listed above.{reset}"
            );
        }
    }
    if let Some(reason) = any_failure {
        return Err(reason.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_ready_rejects_missing_target() {
        let directory = tempfile::tempdir().unwrap();
        assert!(cmd_ready(&directory.path().join("does-not-exist"), false, true).is_err());
    }

    #[test]
    fn ready_check_accepts_info_and_advisories_but_rejects_errors() {
        assert_eq!(
            check_blocking_messages(&["INFO: All refs resolve".into()]).unwrap(),
            0
        );
        assert_eq!(
            check_blocking_messages(&["WARNING: optional contract missing".into()]).unwrap(),
            1
        );
        assert_eq!(
            check_blocking_messages(&["INFO: documentation mentions ERROR:".into()]).unwrap(),
            0
        );
        let failure = check_blocking_messages(&["ERROR: unresolved reference".into()]).unwrap_err();
        assert!(failure.to_string().contains("ERROR: unresolved reference"));
    }
}
