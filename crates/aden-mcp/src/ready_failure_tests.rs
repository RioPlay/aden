// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use serde_json::{Value, json};

fn failed_ready() -> Value {
    let statuses = ["passed", "passed", "failed", "skipped", "skipped"];
    let steps: Vec<_> = [
        "gen",
        "lint",
        "check refs",
        "heal drift scan",
        "owasp audit",
    ]
    .iter()
    .zip(statuses)
    .map(|(name, status)| {
        json!({
            "name": name, "status": status, "passed": status == "passed",
            "reason": "Check /private/project/source.rs", "scope": "local",
            "evidence_kind": "structural_invariant"
        })
    })
    .collect();
    json!({
        "schema_version": 1,
        "command": "ready",
        "ok": false,
        "result_state": "failed",
        "outcome": "blocked",
        "assessment_scope": "local_static_checks",
        "advisory_findings": 0,
        "graph_refresh_requested": true,
        "source_repairs_requested": false,
        "heal_repair_attempted": false,
        "hard_failure": "broken reference in C:\\private\\project\\README.adoc",
        "unverified": ["runtime_behavior", "project_tests", "external_tool_gates"],
        "steps": steps
    })
}

fn envelope(stdout: &str) -> Value {
    let raw = preserve_ready_failure("Error: local checks blocked", stdout);
    serde_json::from_str(&agent_error_for_mcp("ready", &raw)).unwrap()
}

#[test]
fn ready_failure_preserves_exact_steps_without_embedded_json() {
    let original = failed_ready();
    let response = envelope(&original.to_string());
    assert_eq!(response["error"]["code"], "command_failed");
    assert_eq!(response["error"]["safe_to_retry"], false);
    assert_eq!(response["command_result_state"], "present");
    for (before, after) in original["steps"]
        .as_array()
        .unwrap()
        .iter()
        .zip(response["command_result"]["steps"].as_array().unwrap())
    {
        assert_eq!(before["name"], after["name"]);
        assert_eq!(before["status"], after["status"]);
        assert_eq!(before["passed"], after["passed"]);
    }
    assert_eq!(response["error"]["message"], "Error: local checks blocked");
    let text = response.to_string();
    assert!(!text.contains("private"));
    assert!(!text.contains("(stdout)"));
}

#[test]
fn ready_failure_rejects_invalid_mismatched_and_contradictory_stdout() {
    assert_eq!(envelope("{broken")["command_result_state"], "invalid");
    for (field, replacement) in [
        ("command", json!("heal")),
        ("schema_version", json!(2)),
        ("ok", json!(true)),
        ("result_state", json!("complete")),
    ] {
        let mut result = failed_ready();
        result[field] = replacement;
        let response = envelope(&result.to_string());
        assert_eq!(response["command_result_state"], "unsupported");
        assert!(response.get("command_result").is_none());
        assert_eq!(response["error"]["safe_to_retry"], false);
    }
    let mut all_pass = failed_ready();
    for step in all_pass["steps"].as_array_mut().unwrap() {
        step["status"] = "passed".into();
        step["passed"] = true.into();
    }
    assert_eq!(
        envelope(&all_pass.to_string())["command_result_state"],
        "unsupported"
    );
}

#[test]
fn ready_failure_accepts_soft_heal_failure_followed_by_passed_audit() {
    let mut result = failed_ready();
    for (step, status) in result["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(["passed", "passed", "passed", "failed", "passed"])
    {
        step["status"] = status.into();
        step["passed"] = (status == "passed").into();
    }
    assert_eq!(envelope(&result.to_string())["command_result"], {
        redact_command_result(&mut result);
        result
    });
}

#[test]
fn ready_failure_redacts_nested_keys_and_values_preserving_signals() {
    let mut result = failed_ready();
    result["truncated"] = true.into();
    result["response_bytes_truncated"] = true.into();
    result["private_path"] = "C:\\Users\\Private Person\\secret.rs".into();
    result["extra"] =
        json!({"/private/secret": ["\\\\server\\share\\file", "λ語🙂", "aden://module/test"]});
    let response = envelope(&result.to_string());
    assert_eq!(response["command_result"]["truncated"], true);
    assert_eq!(response["command_result"]["response_bytes_truncated"], true);
    assert_eq!(response["command_result"]["private_path"], "<path>");
    assert_eq!(response["command_result"]["extra"]["<path>"][1], "λ語🙂");
    assert_eq!(
        response["command_result"]["extra"]["<path>"][2],
        "aden://module/test"
    );
    assert!(!response.to_string().contains("server"));
}

#[test]
fn ready_failure_redacts_whole_ambiguous_prose_with_spaced_paths() {
    // An unquoted path containing spaces cannot be bounded, so the whole value
    // is withheld rather than leaving `Person\secret.rs` visible.
    for text in [
        r"Read C:\Users\Private Person\secret.rs failed",
        "Read /home/Private Person/secret.rs failed",
        r"Read \\server\Private Person\secret.rs failed",
    ] {
        let response = nested_envelope(text);
        assert_eq!(response["command_result_state"], "present");
        assert_eq!(
            response["command_result"]["nested"],
            json!({"<path>": ["<path>", {"message": "<path>"}]})
        );
        assert!(!response.to_string().contains("Person"));
        assert!(!response.to_string().contains("secret.rs"));
    }
    // A quoted path is delimited by its closing quote, so the diagnostic prose
    // survives while the path stays hidden.
    for (text, expected) in [
        (
            r#"Read "C:\Users\Private Person\secret.rs" failed"#,
            r#"Read "<path>" failed"#,
        ),
        (
            r"Read 'C:\Users\Private Person\secret.rs' failed",
            "Read '<path>' failed",
        ),
        (
            "Read \"/home/Private Person/secret.rs\" failed",
            "Read \"<path>\" failed",
        ),
        (
            "Read '/home/Private Person/secret.rs' failed",
            "Read '<path>' failed",
        ),
    ] {
        let response = nested_envelope(text);
        assert_eq!(response["command_result_state"], "present");
        assert_eq!(
            response["command_result"]["nested"],
            json!({expected: [expected, {"message": expected}]}),
            "prose after a quoted path must survive: {text}"
        );
        assert!(!response.to_string().contains("Person"));
        assert!(!response.to_string().contains("secret.rs"));
    }
}

fn nested_envelope(text: &str) -> serde_json::Value {
    let mut result = failed_ready();
    let mut nested = serde_json::Map::new();
    nested.insert(text.into(), json!([text, {"message": text}]));
    result["nested"] = nested.into();
    envelope(&result.to_string())
}

#[test]
fn ready_failure_bounds_raw_and_serialized_multibyte_escaped_output() {
    let response = envelope(&"x".repeat(FAILED_COMMAND_RESULT_MAX_BYTES + 1));
    assert_eq!(response["command_result_state"], "oversized");
    assert_eq!(response["response_bytes_truncated"], true);
    assert!(response.get("command_result").is_none());
    let mut result = failed_ready();
    result["extra"] = "λ🙂\u{0}\"\\".repeat(2500).into();
    let stderr = "\u{0}λ🙂\"\\".repeat(1500);
    let raw = preserve_ready_failure(&stderr, &result.to_string());
    let response = agent_error_for_mcp("ready", &raw);
    assert!(response.len() <= MCP_RESPONSE_MAX_BYTES);
    let parsed: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(parsed["error"]["safe_to_retry"], false);
    assert!(matches!(
        parsed["command_result_state"].as_str(),
        Some("present" | "oversized")
    ));
}

#[test]
fn ordinary_errors_keep_the_existing_envelope() {
    let raw = preserve_cli_error_for_mcp("Error: check takes no positional arguments");
    let response: Value = serde_json::from_str(&agent_error_for_mcp("ready", &raw)).unwrap();
    assert_eq!(response["error"]["code"], "invalid_args");
    assert!(response.get("command_result").is_none());
    assert!(response.get("command_result_state").is_none());
    assert!(
        ready_failure_for_mcp(
            "heal",
            &preserve_ready_failure("blocked", &failed_ready().to_string())
        )
        .is_none()
    );
}

#[test]
fn source_transport_clipping_invalidates_continuations_and_end_ranges() {
    let text = "λ🙂\r\n".repeat(12_000);
    let continuation = json!({
        "file": "src/example.rs", "start_byte": 100 + text.len(),
        "end_line": 12_000, "expected_hash": "stable", "max_bytes": 8192
    });
    let original = json!({
        "source": {
            "text": text, "state": "clipped", "last_line_complete": true,
            "start_byte": 100, "end_byte": 100 + text.len(),
            "end_line": 12_000, "returned_bytes": text.len(),
            "continuation": continuation
        },
        "continuation": continuation, "next_byte": 100 + text.len(),
        "next_actions": [{"tool": "read", "args": continuation}]
    });
    let bounded =
        enforce_mcp_response_budget("understand", &serde_json::Map::new(), &original.to_string());
    assert!(bounded.len() <= MCP_RESPONSE_MAX_BYTES);
    let response: Value = serde_json::from_str(&bounded).unwrap();
    let returned = response["source"]["text"].as_str().unwrap();
    assert!(returned.len() < text.len());
    assert!(text.starts_with(returned));
    assert_eq!(response["source"]["returned_bytes"], returned.len());
    assert_eq!(response["source"]["last_line_complete"], false);
    assert_eq!(response["incomplete"], true);
    assert_eq!(response["response_bytes_truncated"], true);
    for pointer in [
        "/source/continuation",
        "/source/end_line",
        "/source/end_byte",
        "/continuation",
        "/next_byte",
        "/next_actions",
    ] {
        assert!(response.pointer(pointer).is_none(), "stale {pointer}");
    }
}

#[test]
fn source_transport_preserves_valid_continuation_without_text_clipping() {
    let original = json!({
        "source": {
            "text": "λ🙂\r\n", "state": "clipped", "last_line_complete": true,
            "end_line": 1, "continuation": {"start_byte": 108, "expected_hash": "stable"}
        }
    });
    let bounded =
        enforce_mcp_response_budget("understand", &serde_json::Map::new(), &original.to_string());
    assert_eq!(serde_json::from_str::<Value>(&bounded).unwrap(), original);
}
