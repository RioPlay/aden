//! Repair mode must preserve machine output and reject remaining lint errors.

use std::process::{Command, Output};

fn fixture(source: &[u8]) -> (tempfile::TempDir, tempfile::TempDir) {
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname=\"lint-repair-fixture\"\nversion=\"0.1.0\"\nedition=\"2024\"\n",
    )
    .unwrap();
    std::fs::write(project.path().join("probe.py"), source).unwrap();
    (project, data)
}

fn run(project: &tempfile::TempDir, data: &tempfile::TempDir, command: &str) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_aden"));
    process
        .arg(command)
        .arg(project.path())
        .args(["--fix", "--json"])
        .env("ADEN_DATA_DIR", data.path())
        .env_remove("ADEN_SKIP_AUTO_GEN");
    if command == "lint" {
        process.args(["--severity", "Error"]);
    }
    process.output().unwrap()
}

#[test]
fn clean_lint_fix_is_one_json_value_and_preserves_source() {
    let source = b"answer = 4\r\n";
    let (project, data) = fixture(source);
    let output = run(&project, &data, "lint");
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result, serde_json::json!([]));
    assert_eq!(
        std::fs::read(project.path().join("probe.py")).unwrap(),
        source
    );
}

#[test]
fn lint_fix_rejects_unrepairable_errors_without_changing_source() {
    let source = b"answer = eval('2 + 2')\r\n";
    let (project, data) = fixture(source);
    let output = run(&project, &data, "lint");
    assert!(!output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["rule"] == "eval_usage" && finding["severity"] == "Error" })
    );
    assert_eq!(
        std::fs::read(project.path().join("probe.py")).unwrap(),
        source
    );
}

#[test]
fn ready_fix_rejects_remaining_lint_error_with_one_json_envelope() {
    let source = b"answer = eval('2 + 2')\r\n";
    let (project, data) = fixture(source);
    let output = run(&project, &data, "ready");
    assert!(!output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["command"], "ready");
    assert_eq!(result["ok"], false);
    let steps = result["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|step| step["name"] == "lint" && step["status"] == "failed")
    );
    assert!(
        steps
            .iter()
            .any(|step| step["name"] == "check refs" && step["status"] == "skipped")
    );
    assert_eq!(
        std::fs::read(project.path().join("probe.py")).unwrap(),
        source
    );
}
