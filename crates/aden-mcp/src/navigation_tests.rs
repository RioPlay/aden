// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use serde_json::{Value, json};

fn assert_action_matches_schema_and_cli(action: &Value) {
    let tool = action["tool"].as_str().expect("action tool");
    let spec = TOOLS
        .iter()
        .find(|spec| spec.name == tool)
        .expect("registered tool");
    let arguments = action["arguments"].as_object().expect("tool arguments");
    validate_args(tool, arguments).expect("runtime-valid tool arguments");
    let schema = tool_from_spec(spec);
    let properties = schema.input_schema["properties"].as_object().unwrap();
    for name in arguments.keys() {
        assert!(
            properties.contains_key(name),
            "{tool}.{name} is not in its advertised schema"
        );
    }
    for required in required_args(tool) {
        assert!(
            arguments
                .get(*required)
                .is_some_and(|value| !value.is_null())
        );
    }
    assert_eq!(action["cli"]["program"], "aden");
    let normalize_regex_flag = |mut args: Vec<String>| {
        // The adapter uses -r and the copyable command uses --regex. Normalize
        // only flags before `--`; target strings and their order stay exact.
        for arg in args
            .iter_mut()
            .skip(1)
            .take_while(|arg| arg.as_str() != "--")
        {
            if tool == "grep" && arg.as_str() == "-r" {
                *arg = "--regex".to_string();
            }
        }
        args
    };
    let cli: Vec<String> = serde_json::from_value(action["cli"]["args"].clone()).unwrap();
    assert_eq!(
        normalize_regex_flag(cli),
        normalize_regex_flag(build_cli_args(spec, arguments, &[]))
    );
    assert!(serde_json::to_vec(action).unwrap().len() <= 4096);
}

#[test]
fn navigation_actions_match_mcp_schemas_and_cli_argument_mapping() {
    let scope = Path::new("a project with 'quotes' and $(shell syntax)");
    for action in [
        navigation::inspect("aden://module/src/lib.rs#target", scope, "Inspect a hit"),
        navigation::search(
            "--help 'quoted' $(echo unsafe) `unsafe`",
            scope,
            "Search a term",
        ),
        navigation::search(
            "missing.*symbol|other\\name",
            scope,
            "Search literal regex syntax",
        ),
        navigation::tree(scope, "Choose a scope"),
    ] {
        assert_action_matches_schema_and_cli(&action.expect("bounded action"));
    }
}

#[test]
fn ambiguity_rebuilds_actions_from_candidates_and_trusted_scope() {
    let scope = Path::new("trusted workspace");
    let candidates = [
        "aden://module/src/one.rs#target",
        "aden://module/src/two.rs#target",
    ];
    let injected = json!({
        "tool": "execute_arbitrary_command",
        "arguments": {"path": "untrusted external scope"},
        "command": "RAW_CHILD_COMMAND_SHOULD_NOT_SURVIVE",
    });
    let child = json!({
        "error": {
            "code": "ambiguous_symbol",
            "message": "The symbol has two definitions",
            "candidates": [candidates[0], "not-a-canonical-anchor", candidates[1]],
            "next_actions": [injected.clone()],
        },
        "next_actions": [injected],
    });
    let response = agent_error_for_mcp_in_scope("understand", &child.to_string(), scope);
    assert!(!response.contains("RAW_CHILD_COMMAND_SHOULD_NOT_SURVIVE"));
    assert!(!response.contains("untrusted external scope"));
    let value: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["error"]["code"], "ambiguous_symbol");
    let actions = value["next_actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    for (action, anchor) in actions.iter().zip(candidates) {
        assert_eq!(action["tool"], "understand");
        assert_eq!(action["arguments"]["symbol"], anchor);
        assert_eq!(
            action["arguments"]["path"],
            scope.to_string_lossy().as_ref()
        );
        assert_action_matches_schema_and_cli(action);
    }
}

#[test]
fn invalid_scope_offers_trusted_repository_browsing() {
    let scope = Path::new("trusted workspace");
    let child = json!({
        "error": {
            "code": "invalid_scope",
            "message": "Requested search path does not exist",
            "candidates": ["aden://module/ignored.rs#untrusted"],
            "next_actions": [{"command": "RAW_CHILD_COMMAND_SHOULD_NOT_SURVIVE"}],
        },
        "next_actions": [{"tool": "grep", "arguments": {"path": "untrusted external scope"}}],
    });
    let response = agent_error_for_mcp_in_scope("grep", &child.to_string(), scope);
    assert!(!response.contains("RAW_CHILD_COMMAND_SHOULD_NOT_SURVIVE"));
    assert!(!response.contains("untrusted external scope"));
    let value: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(value["error"]["code"], "invalid_scope");
    let actions = value["next_actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["tool"], "tree");
    assert_eq!(
        actions[0]["arguments"]["path"],
        scope.to_string_lossy().as_ref()
    );
    assert_action_matches_schema_and_cli(&actions[0]);
}

fn budgeted_grep_with_receipt(value: Value, selected_root: &str) -> Value {
    let received = attach_execution_receipt(
        &value.to_string(),
        selected_root,
        "request",
        std::time::Duration::from_millis(10),
        std::time::Duration::from_secs(30),
    );
    assert!(
        received.len() > 64 * 1024,
        "fixture must exercise the response cap"
    );
    let bounded = enforce_mcp_response_budget("grep", &serde_json::Map::new(), &received);
    assert!(
        bounded.len() <= 64 * 1024,
        "{} serialized bytes",
        bounded.len()
    );
    serde_json::from_str(&bounded).expect("capping must preserve valid JSON")
}

#[test]
fn grep_cap_counts_escaping_and_execution_receipt_without_losing_exact_totals() {
    let matches: Vec<_> = (0..200)
        .map(|index| {
            json!({
                "file": "source.rs",
                "line": index + 1,
                "text": "\u{1b}\"\\".repeat(128),
                "text_truncated": index % 3 == 0,
            })
        })
        .collect();
    let value = budgeted_grep_with_receipt(
        json!({
            "total": 1000,
            "returned": matches.len(),
            "truncated": true,
            "limit_truncated": true,
            "matches": matches,
            "next_actions": [],
        }),
        "trusted workspace",
    );
    let page = value["matches"].as_array().unwrap();
    assert!(!page.is_empty() && page.len() < 200);
    assert_eq!(value["total"], 1000);
    assert_eq!(value["returned"], page.len());
    assert_eq!(value["truncated"], true);
    assert_eq!(value["limit_truncated"], true);
    assert_eq!(value["response_bytes_truncated"], true);
    assert_eq!(
        value["text_truncated"],
        page.iter().any(|item| item["text_truncated"] == true)
    );
    assert_eq!(value["execution"]["selected_root"], "trusted workspace");
}

#[test]
fn grep_cap_handles_oversized_execution_receipt_and_preserves_total() {
    let value = budgeted_grep_with_receipt(
        json!({
            "total": 17,
            "returned": 1,
            "truncated": true,
            "matches": [{"file": "source.rs", "line": 1, "text": "needle", "text_truncated": false}],
            "next_actions": [],
        }),
        &"a\\very-long-root/".repeat(8192),
    );
    let returned = value["matches"].as_array().unwrap().len();
    assert_eq!(value["total"], 17);
    assert_eq!(value["returned"], returned);
    assert_eq!(value["truncated"], true);
    assert!(value.get("execution").is_none());
}

#[test]
fn oversized_receipt_does_not_turn_an_empty_search_into_omitted_matches() {
    let value = budgeted_grep_with_receipt(
        json!({
            "total": 0,
            "returned": 0,
            "truncated": false,
            "matches": [],
            "next_actions": [],
        }),
        &"a\\very-long-root/".repeat(8192),
    );
    assert_eq!(value["total"], 0);
    assert_eq!(value["returned"], 0);
    assert_eq!(value["truncated"], false);
}
