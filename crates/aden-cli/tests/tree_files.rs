// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Project {
    project: tempfile::TempDir,
    data: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let project = tempfile::Builder::new()
            .prefix("aden-tree-files-")
            .tempdir()
            .unwrap();
        let git = Command::new("git")
            .args(["init", "--quiet", "--template="])
            .arg(project.path())
            .output()
            .expect("git must be available");
        assert!(
            git.status.success(),
            "{}",
            String::from_utf8_lossy(&git.stderr)
        );
        Self {
            project,
            data: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.project.path()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aden"))
            .current_dir(self.path())
            .env("ADEN_DATA_DIR", self.data.path())
            .args(args)
            .output()
            .expect("aden binary must be built")
    }
}

fn payload(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn docs_only_files_are_listed_with_project_relative_scope_and_zero_symbols() {
    let project = Project::new();
    std::fs::create_dir_all(project.path().join("docs")).unwrap();
    std::fs::write(project.path().join("README.md"), "Project overview\n").unwrap();
    std::fs::write(project.path().join("docs/guide.adoc"), "= Guide\n").unwrap();
    std::fs::write(project.path().join("settings.toml"), "enabled = true\n").unwrap();
    std::fs::write(project.path().join("ignored.png"), [0, 1, 2]).unwrap();
    std::fs::write(project.path().join("hidden.md"), "excluded by policy\n").unwrap();
    std::fs::write(project.path().join(".adenignore"), "hidden.md\n").unwrap();
    std::fs::create_dir_all(project.path().join("target")).unwrap();
    std::fs::write(
        project.path().join("target/generated.rs"),
        "fn ignored() {}\n",
    )
    .unwrap();

    let result = payload(project.run(&["tree", "--files", "."]));
    assert_eq!(result["format"], "files-v1");
    assert_eq!(result["scope"], ".");
    assert_eq!(result["file_count"], 3);
    assert_eq!(result["returned_file_count"], 3);
    assert_eq!(result["symbol_count"], 0);
    assert_eq!(result["returned_symbol_count"], 0);
    let outline = result["outline"].as_str().unwrap();
    for file in ["README.md", "docs/guide.adoc", "settings.toml"] {
        assert!(outline.contains(file), "missing {file} in {outline}");
    }
    assert!(!outline.contains("ignored.png"), "{outline}");
    assert!(!outline.contains("hidden.md"), "{outline}");
    assert!(!outline.contains("target/generated.rs"), "{outline}");
    assert!(
        !outline.contains(project.path().to_string_lossy().as_ref()),
        "paths must be relative: {outline}"
    );

    let scoped = payload(project.run(&["tree", "--files", "docs"]));
    assert_eq!(scoped["scope"], "docs");
    assert_eq!(scoped["file_count"], 1);
    assert!(
        scoped["outline"]
            .as_str()
            .unwrap()
            .contains("docs/guide.adoc")
    );
}

#[test]
fn narrowed_trees_preserve_root_discovery_policy() {
    let project = Project::new();
    let folder = project.path().join("config");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("settings.toml"), "# visible\n").unwrap();
    std::fs::write(folder.join("credentials.toml"), "# synthetic fixture\n").unwrap();

    for scope in [".", "config", "config/credentials.toml"] {
        let result = payload(project.run(&["tree", "--files", "--unlimited", scope]));
        let outline = result["outline"].as_str().unwrap();
        assert!(!outline.contains("credentials.toml"), "{scope}: {outline}");
        if scope.ends_with("credentials.toml") {
            assert_eq!(result["file_count"], 0);
        } else {
            assert!(outline.contains("config/settings.toml"));
        }
    }
    let search = payload(project.run(&["grep", "synthetic fixture", "config"]));
    assert_eq!(search["total"], 1);
    assert_eq!(search["matches"][0]["file"], "config/credentials.toml");
}

#[test]
fn bare_filename_in_manifest_project_stays_relative() {
    let project = Project {
        project: tempfile::tempdir().unwrap(),
        data: tempfile::tempdir().unwrap(),
    };
    std::fs::write(project.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    std::fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();

    let bare = payload(project.run(&["tree", "--files", "main.rs"]));
    let explicit = payload(project.run(&["tree", "--files", "./main.rs"]));
    assert_eq!(bare["scope"], "main.rs");
    assert_eq!(bare["outline"], "# 1 discovered files\nmain.rs\n");
    assert_eq!(bare["outline"], explicit["outline"]);
}

#[test]
fn mixed_project_file_map_includes_prose_configuration_and_code() {
    let project = Project::new();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("README.md"), "Mixed project\n").unwrap();
    std::fs::write(
        project.path().join("Cargo.toml"),
        "[package]\nname='sample'\n",
    )
    .unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn hello() {}\n").unwrap();

    let result = payload(project.run(&["tree", "--files", "."]));
    let outline = result["outline"].as_str().unwrap();
    for file in ["README.md", "Cargo.toml", "src/lib.rs"] {
        assert!(outline.contains(file), "missing {file} in {outline}");
    }
    assert_eq!(result["file_count"], 3);
    assert_eq!(result["symbol_count"], 0);

    let symbols = payload(project.run(&["tree", "."]));
    assert_eq!(symbols["format"], "symbol-outline-v1");
    assert!(symbols["outline"].as_str().unwrap().contains("hello"));
    assert!(!symbols["outline"].as_str().unwrap().contains("README.md"));
}

#[test]
fn bounded_file_list_reports_totals_and_replayable_files_actions() {
    let project = Project::new();
    for index in 0..400 {
        let dir = format!("group-{index:04}-abcdefghijklmnop");
        let path: PathBuf = project.path().join(&dir);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("guide.md"), "Documentation\n").unwrap();
    }

    let clipped = payload(project.run(&["tree", "--files", "."]));
    assert_eq!(clipped["result_state"], "truncated");
    assert_eq!(clipped["truncated"], true);
    assert_eq!(clipped["file_count"], 400);
    assert!(clipped["returned_file_count"].as_u64().unwrap() < 400);
    assert!(clipped["outline"].as_str().unwrap().len() <= 12 * 1024);
    assert!(clipped["next_action"].as_str().unwrap().contains("--files"));
    let actions = clipped["next_actions"].as_array().unwrap();
    assert!(!actions.is_empty());
    for action in actions {
        let args = action["cli"]["args"].as_array().unwrap();
        assert!(args.iter().any(|arg| arg == "--files"));
        assert!(action["arguments"].get("files").is_none());
        assert_eq!(action["arguments"]["symbols"], false);
        assert!(action["command"].as_str().unwrap().contains("--files"));
        let replay: Vec<_> = args.iter().map(|arg| arg.as_str().unwrap()).collect();
        let replayed = payload(project.run(&replay));
        assert_eq!(replayed["format"], "files-v1");
    }

    let full = payload(project.run(&["--unlimited", "tree", "--files", "."]));
    assert_eq!(full["result_state"], "complete");
    assert_eq!(full["file_count"], 400);
    assert_eq!(full["returned_file_count"], 400);
    assert_eq!(full["truncated"], false);
}

#[test]
fn human_file_mode_lists_paths_and_conflicting_modes_are_rejected() {
    let project = Project::new();
    std::fs::write(project.path().join("README.md"), "Docs\n").unwrap();

    let human = project.run(&["--human", "tree", "--files", "."]);
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(!stdout.trim_start().starts_with('{'), "{stdout}");
    assert!(stdout.contains("README.md"), "{stdout}");

    let conflict = project.run(&["tree", "--files", "--symbols", "."]);
    assert!(!conflict.status.success());
}
