// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Exercise Antigravity registration through the real CLI in temporary projects.
//! Global lifecycle coverage is Unix-only: Windows resolves the actual home via
//! Known Folder APIs, so overriding HOME/USERPROFILE does not isolate that scope.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    caller: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("isolated home");
        let caller = root.path().join("caller");
        let project = root.path().join("target project");
        for path in [&home, &caller, &project] {
            std::fs::create_dir_all(path).unwrap();
        }
        Self {
            _root: root,
            home,
            caller,
            project,
        }
    }

    fn global_config(&self) -> PathBuf {
        self.home.join(".gemini/config/mcp_config.json")
    }

    fn project_config(&self) -> PathBuf {
        self.project.join(".agents/mcp_config.json")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .arg("mcp")
            .args(args)
            .current_dir(&self.caller)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("APPDATA", self.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData/Local"))
            .env("ADEN_DATA_DIR", self.home.join("aden-data"))
            .env("ADEN_SKIP_AUTO_GEN", "1")
            .output()
            .unwrap()
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "args={args:?}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

fn write_config(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn read_config(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn assert_launch_config(entry: &Value, args: Value) {
    let command = entry["command"].as_str().unwrap();
    assert_eq!(
        Path::new(command).canonicalize().unwrap(),
        Path::new(env!("CARGO_BIN_EXE_aden"))
            .canonicalize()
            .unwrap()
    );
    // Exact equality also excludes implicit trust, approval, or cwd pinning.
    assert_eq!(entry, &json!({ "command": command, "args": args }));
}

#[test]
#[cfg(unix)]
fn antigravity_default_install_is_global_idempotent_and_listed() {
    let fixture = Fixture::new();
    let global = fixture.global_config();
    // Prove home isolation before any global command is allowed to write config.
    let preview = fixture.success(&["install", "--platform", "antigravity", "--dry-run"]);
    assert!(preview.contains(&global.display().to_string()));
    assert!(!global.exists());
    let original = json!({
        "mcpServers": { "other": { "command": "other-server", "args": ["serve"] } },
        "unrelatedSetting": { "enabled": false }
    });
    write_config(
        &global,
        serde_json::to_string(&original).unwrap().as_bytes(),
    );
    let local = fixture.caller.join(".agents/mcp_config.json");
    let local_bytes = br#"{"mcpServers":{"local-only":{"command":"local-server"}}}"#;
    write_config(&local, local_bytes);
    let legacy = fixture.home.join(".gemini/antigravity/mcp_config.json");
    let legacy_bytes = br#"{"mcpServers":{"old":{"command":"legacy-server"}}}"#;
    write_config(&legacy, legacy_bytes);

    let installed = fixture.success(&["install", "--platform", "antigravity"]);
    assert!(installed.contains("Antigravity (user scope)"));
    let config = read_config(&global);
    assert_launch_config(&config["mcpServers"]["aden"], json!(["mcp", "stdio"]));
    assert_eq!(
        config["mcpServers"]["other"],
        original["mcpServers"]["other"]
    );
    assert_eq!(config["unrelatedSetting"], original["unrelatedSetting"]);
    let first_install = std::fs::read(&global).unwrap();
    fixture.success(&["install", "--platform", "antigravity"]);
    assert_eq!(std::fs::read(&global).unwrap(), first_install);

    let listing = fixture.success(&["list"]);
    let row = listing
        .lines()
        .find(|line| line.starts_with("Antigravity"))
        .unwrap();
    let mut columns = row.split_whitespace();
    assert_eq!(columns.next(), Some("Antigravity"));
    assert_eq!(columns.next(), Some("✓"));
    assert_eq!(columns.next(), Some("✓"));
    assert!(row.contains(&global.display().to_string()));

    fixture.success(&["uninstall", "--platform", "antigravity"]);
    assert_eq!(read_config(&global), original);
    fixture.success(&["uninstall", "--platform", "antigravity"]);
    assert_eq!(read_config(&global), original);
    assert_eq!(std::fs::read(&local).unwrap(), local_bytes);
    assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
}

#[test]
fn antigravity_project_override_and_named_surface_stay_in_selected_scope() {
    let fixture = Fixture::new();
    let global = fixture.global_config();
    let global_bytes = br#"{"mcpServers":{"aden-review":{"command":"global-server"}}}"#;
    write_config(&global, global_bytes);
    let project = fixture.project.to_str().unwrap();
    let selected = fixture.project_config();
    let original = json!({
        "mcpServers": { "aden": { "command": "existing-server" }, "other": { "command": "other" } },
        "unrelatedSetting": "preserve"
    });
    write_config(
        &selected,
        serde_json::to_string(&original).unwrap().as_bytes(),
    );

    let install_args = [
        "install",
        "--platform",
        "antigravity",
        "--scope",
        "project",
        "--project",
        project,
        "--name",
        "aden-review",
        "--surface",
        "standard",
    ];
    fixture.success(&install_args);
    let config = read_config(&selected);
    assert_launch_config(
        &config["mcpServers"]["aden-review"],
        json!(["mcp", "stdio", "--surface", "standard"]),
    );
    let mut expected = original.clone();
    expected["mcpServers"]["aden-review"] = config["mcpServers"]["aden-review"].clone();
    assert_eq!(config, expected);
    let before_uninstall = std::fs::read(&selected).unwrap();
    fixture.success(&install_args);
    assert_eq!(std::fs::read(&selected).unwrap(), before_uninstall);
    fixture.success(&[
        "uninstall",
        "--platform",
        "antigravity",
        "--scope",
        "project",
        "--project",
        project,
        "--name",
        "aden-review",
        "--dry-run",
    ]);
    assert_eq!(std::fs::read(&selected).unwrap(), before_uninstall);
    let uninstall_args = [
        "uninstall",
        "--platform",
        "antigravity",
        "--scope",
        "project",
        "--project",
        project,
        "--name",
        "aden-review",
    ];
    fixture.success(&uninstall_args);
    assert_eq!(read_config(&selected), original);
    fixture.success(&uninstall_args);
    assert_eq!(read_config(&selected), original);
    assert_eq!(std::fs::read(&global).unwrap(), global_bytes);
    assert!(!fixture.caller.join(".agents").exists());
}

#[test]
fn antigravity_dry_run_does_not_create_or_rewrite_configuration() {
    let fixture = Fixture::new();
    let project = fixture.project.to_str().unwrap();
    fixture.success(&[
        "install",
        "--platform",
        "antigravity",
        "--scope",
        "project",
        "--project",
        project,
        "--dry-run",
    ]);
    assert!(!fixture.project.join(".agents").exists());
    assert!(!fixture.home.join(".gemini").exists());
    let selected = fixture.project_config();
    let original =
        b"{\n  \"mcpServers\": {\"aden\": {\"command\": \"old\"}},\n  \"keep\": true\n}\n";
    write_config(&selected, original);
    for action in ["install", "uninstall"] {
        fixture.success(&[
            action,
            "--platform",
            "antigravity",
            "--scope",
            "project",
            "--project",
            project,
            "--dry-run",
        ]);
        assert_eq!(std::fs::read(&selected).unwrap(), original);
        assert!(!selected.with_extension("tmp").exists());
    }
}

#[test]
fn antigravity_rejects_unsupported_json_without_changing_original_bytes() {
    let fixture = Fixture::new();
    let selected = fixture.project_config();
    let project = fixture.project.to_str().unwrap();
    let unsupported = [
        "{broken",
        "{ // retained comment\n\"mcpServers\": {}}",
        "{\"mcpServers\": {},}",
        "\u{feff}{\"mcpServers\": {}}",
    ];
    for contents in unsupported {
        write_config(&selected, contents.as_bytes());
        for action in ["install", "uninstall"] {
            let output = fixture.run(&[
                action,
                "--platform",
                "antigravity",
                "--scope",
                "project",
                "--project",
                project,
            ]);
            assert!(!output.status.success(), "{action} accepted {contents:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("Invalid JSON"), "{stderr}");
            assert_eq!(std::fs::read(&selected).unwrap(), contents.as_bytes());
            assert!(!selected.with_extension("tmp").exists());
        }
    }
}
