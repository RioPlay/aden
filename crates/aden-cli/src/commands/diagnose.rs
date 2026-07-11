// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic diagnostic scanner for knowledge graphs.
//!
//! Scans a directory for structural issues in documents and the
//! knowledge graph, reporting them with severity, file location, and
//! actionable suggestions.

use std::path::Path;

/// Run a diagnostic scan on the given directory.
///
/// Returns human-readable text output by default, or JSON if `format == "json"`.
pub fn cmd_diagnose(path: &Path, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let rules = aden_diagnose::DiagnosticRules::default();
    let policy = aden_policy::audit_policy(path);

    match format {
        "json" => {
            let diagnosis = aden_diagnose::diagnose_with_rules(path, &rules)?;
            let outcome = crate::commands::outcome::OutcomeEnvelope::evaluated(
                diagnosis.error_count,
                diagnosis.warning_count + diagnosis.info_count + policy.violations.len(),
                if diagnosis.error_count > 0 {
                    "unhealthy"
                } else {
                    "healthy"
                },
                crate::commands::outcome::policy_label(policy.violations.len(), policy.unwired),
                "not_evaluated",
            );
            let mut value = serde_json::to_value(&diagnosis)?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("policy_mode".to_string(), serde_json::json!(policy.mode));
                obj.insert(
                    "policy_unwired".to_string(),
                    serde_json::json!(policy.unwired),
                );
                obj.insert(
                    "policy_violations".to_string(),
                    serde_json::json!(policy.violations),
                );
                obj.insert("result".to_string(), serde_json::to_value(outcome)?);
            }
            println!("{}", serde_json::to_string(&value)?);
        }
        _ => {
            let diagnosis = aden_diagnose::diagnose_with_rules(path, &rules)?;
            print_diagnosis(&diagnosis);
            print_policy_advisory(&policy);
        }
    }

    Ok(())
}

fn print_policy_advisory(policy: &aden_policy::PolicyAudit) {
    println!("== POLICY ({} mode) ==", policy.mode);
    if !policy.constitution_present {
        println!("  INFO: no .aden/constitution.adoc (optional)");
        return;
    }
    if policy.unwired {
        println!(
            "  WARN: constitution present but {} directive(s) loaded — policy engine may be unwired",
            policy.directive_count
        );
    } else {
        println!(
            "  INFO: {} constitution directive(s) loaded",
            policy.directive_count
        );
    }
    for v in &policy.violations {
        println!("  [{}] {} ({})", v.severity, v.message, v.source);
    }
    println!();
}

/// Print a human-readable diagnosis report.
fn print_diagnosis(diagnosis: &aden_diagnose::Diagnosis) {
    let total = diagnosis.issues.len();
    let score = diagnosis.health_score;

    println!("Aden Diagnose: {}", score as i32);
    println!(
        "Issues: {} errors, {} warnings, {} info ({} total)",
        diagnosis.error_count, diagnosis.warning_count, diagnosis.info_count, total
    );
    println!();

    if diagnosis.issues.is_empty() {
        println!("No issues found. Graph is healthy.");
        return;
    }

    // Group by category
    let mut by_category: std::collections::HashMap<String, Vec<&aden_diagnose::Issue>> =
        std::collections::HashMap::new();
    for issue in &diagnosis.issues {
        by_category
            .entry(format!("{}", issue.category))
            .or_default()
            .push(issue);
    }

    // Cap how many issues are printed per category so a large repo's expected
    // metadata (e.g. hundreds of orphan reference docs at Info severity) can't
    // flood an agent's context past the token cap. The total count is always shown
    // in the category header; use `--format json` for the complete machine-readable
    // list. Errors/warnings are never the bulk here, but the cap applies uniformly.
    const PER_CATEGORY_CAP: usize = 10;
    for (category, issues) in &by_category {
        println!(
            "== {} ({} issues)",
            category.to_uppercase().replace('-', " "),
            issues.len()
        );
        for issue in issues.iter().take(PER_CATEGORY_CAP) {
            let file = issue
                .file
                .as_deref()
                .map(|f| format!(" ({})", f))
                .unwrap_or_default();
            let line = issue.line.map(|l| format!(":{}", l)).unwrap_or_default();
            let sev = match issue.severity {
                aden_diagnose::Severity::Error => "ERROR",
                aden_diagnose::Severity::Warning => "WARN",
                aden_diagnose::Severity::Info => "INFO",
            };
            println!("  [{}] {}{}{}", sev, issue.message, file, line);
            if let Some(suggestion) = &issue.suggestion {
                println!("    -> {}", suggestion);
            }
        }
        if issues.len() > PER_CATEGORY_CAP {
            println!(
                "  ... and {} more (use --format json for the full list)",
                issues.len() - PER_CATEGORY_CAP
            );
        }
        println!();
    }
}
