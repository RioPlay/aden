// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn json_args(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().expect("object arguments").clone()
}

#[test]
fn generation_tools_have_the_accepted_surface_and_effects() {
    let generation = TOOLS.iter().find(|spec| spec.name == "gen").unwrap();
    let status = TOOLS
        .iter()
        .find(|spec| spec.name == "generation_status")
        .unwrap();
    let start = TOOLS
        .iter()
        .find(|spec| spec.name == "generation_start")
        .unwrap();
    let cancel = TOOLS
        .iter()
        .find(|spec| spec.name == "generation_cancel")
        .unwrap();

    assert_eq!(tool_tier(status.name), Tier::Essential);
    assert!(status.effect == Effect::Read);
    assert_eq!(tool_tier(start.name), Tier::Full);
    assert!(start.effect == Effect::Mutate);
    assert_eq!(tool_tier(cancel.name), Tier::Full);
    assert!(cancel.effect == Effect::Mutate);
    assert!(tool_enabled("generation_status", Tier::Essential));
    assert_eq!(tool_tier(generation.name), Tier::Full);
    assert!(generation.effect == Effect::Mutate);
    assert!(!tool_enabled("generation_start", Tier::Standard));
    assert!(tool_enabled("generation_start", Tier::Full));
}

#[test]
fn generation_cli_argv_is_narrow_and_machine_readable() {
    let path = "C:/work/sample";
    assert_eq!(
        prepare_cli_args_for_mcp("gen", &json_args(serde_json::json!({"path": path}))).unwrap(),
        ["gen", path, "--background", "--json"]
    );
    assert_eq!(
        prepare_cli_args_for_mcp(
            "generation_status",
            &json_args(serde_json::json!({"path": path}))
        )
        .unwrap(),
        ["jobs", "status", path, "--json"]
    );
    assert_eq!(
        prepare_cli_args_for_mcp(
            "generation_start",
            &json_args(serde_json::json!({"path": path}))
        )
        .unwrap(),
        ["gen", path, "--background", "--json"]
    );
    assert_eq!(
        prepare_cli_args_for_mcp(
            "generation_cancel",
            &json_args(serde_json::json!({"path": path, "job_id": "01234567-abcd"}))
        )
        .unwrap(),
        [
            "jobs",
            "cancel",
            path,
            "--job-id",
            "01234567-abcd",
            "--json"
        ]
    );
}

#[test]
fn generation_tools_reject_untrusted_or_mistyped_arguments() {
    assert!(
        prepare_cli_args_for_mcp(
            "generation_start",
            &json_args(serde_json::json!({"path": "C:/work/sample", "force": true}))
        )
        .is_err()
    );
    assert!(
        prepare_cli_args_for_mcp(
            "generation_status",
            &json_args(serde_json::json!({"path": 3}))
        )
        .is_err()
    );
    assert!(
        prepare_cli_args_for_mcp(
            "generation_status",
            &json_args(serde_json::json!({"path": "--force"}))
        )
        .is_err()
    );
    let too_long = "a".repeat(81);
    let max_length = "a".repeat(80);
    assert!(
        prepare_cli_args_for_mcp(
            "generation_cancel",
            &json_args(serde_json::json!({"path": "C:/work/sample", "job_id": max_length}))
        )
        .is_ok()
    );
    for token in ["", "bad token", "xyz", too_long.as_str()] {
        assert!(
            prepare_cli_args_for_mcp(
                "generation_cancel",
                &json_args(serde_json::json!({"path": "C:/work/sample", "job_id": token}))
            )
            .is_err()
        );
    }
}

#[test]
fn legacy_gen_options_fail_with_a_clear_background_only_error() {
    for option in ["auto", "quiet", "propose", "force_regen", "force", "reset"] {
        let args = json_args(serde_json::json!({"path": "C:/work/sample", (option): true}));
        let error = validate_args("gen", &args).unwrap_err();
        assert!(
            error.contains(option),
            "missing option name in error: {error}"
        );
        assert!(error.contains("durable background job"));
    }
}

#[test]
fn status_json_keeps_pending_and_terminal_states_distinct() {
    for (tool, raw) in [
        ("gen", "null"),
        ("gen", r#"{"state":"queued","phase":"waiting"}"#),
        ("gen", r#"{"state":"running","phase":"indexing"}"#),
        ("gen", r#"{"state":"completed","phase":"done"}"#),
        ("gen", r#"{"state":"failed","phase":"error"}"#),
        ("generation_status", "null"),
        (
            "generation_status",
            r#"{"state":"queued","phase":"waiting"}"#,
        ),
        (
            "generation_status",
            r#"{"state":"running","phase":"indexing"}"#,
        ),
        (
            "generation_status",
            r#"{"state":"completed","phase":"done"}"#,
        ),
        ("generation_status", r#"{"state":"failed","phase":"error"}"#),
    ] {
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        let forwarded: serde_json::Value = serde_json::from_str(&redact_generation_output(
            &agent_response_for_mcp(tool, raw),
        ))
        .unwrap();
        assert_eq!(forwarded, parsed, "status payload was reinterpreted: {raw}");
    }

    let with_root = r#"{"state":"queued","root":"C:/private/work"}"#;
    let forwarded: serde_json::Value =
        serde_json::from_str(&redact_generation_output(with_root)).unwrap();
    assert_eq!(forwarded["state"], "queued");
    assert_eq!(forwarded["root"], "<path>");
}

#[test]
fn job_command_errors_follow_read_and_mutation_retry_rules() {
    let status: serde_json::Value =
        serde_json::from_str(&agent_error_for_mcp("generation_status", "read failed")).unwrap();
    let start: serde_json::Value =
        serde_json::from_str(&agent_error_for_mcp("generation_start", "start failed")).unwrap();
    let cancel: serde_json::Value =
        serde_json::from_str(&agent_error_for_mcp("generation_cancel", "cancel failed")).unwrap();
    let generation: serde_json::Value =
        serde_json::from_str(&agent_error_for_mcp("gen", "generation start failed")).unwrap();
    assert_eq!(status["error"]["safe_to_retry"], true);
    assert_eq!(start["error"]["safe_to_retry"], false);
    assert_eq!(cancel["error"]["safe_to_retry"], false);
    assert_eq!(generation["error"]["safe_to_retry"], false);
}
