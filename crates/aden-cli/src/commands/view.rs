// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden view` — render a graph slice as an interactive, *offline* browser page.
//!
//! Reuses `viz`'s JSON slice (so the viewer can never diverge from the text
//! formats), inlines it *and* the vendored force-graph library into a single
//! self-contained HTML (`const DATA = {…}` — no runtime `fetch`, so it works from
//! `file://`), writes it out, and opens it in the default browser. The aden core
//! stays renderer-free: the page is a pure JSON consumer. Gated behind the `view`
//! cargo feature so default builds carry none of the embedded JS.

use std::path::{Path, PathBuf};

/// Vendored, pinned, sha256-verified force-graph UMD bundle (MIT). See
/// `assets/CHECKSUMS` and `NOTICE.md`. Embedded so the page is fully offline.
const FORCE_GRAPH_JS: &str = include_str!("../../assets/force-graph.min.js");
/// Self-contained page template with `/*FORCE_GRAPH_LIB*/` and `/*ADEN_DATA*/`
/// placeholders (string-replaced, not `format!`, to avoid brace conflicts).
const VIEW_HTML: &str = include_str!("../../assets/view.html");

// A CLI command handler — its parameters mirror the subcommand's flags 1:1, so a
// bundle struct would only add indirection.
#[allow(clippy::too_many_arguments)]
pub fn cmd_view(
    path: &Path,
    anchor: Option<&str>,
    mode: &str,
    depth: usize,
    threed: bool,
    open: bool,
    out: Option<&Path>,
    editor: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if threed {
        return Err(
            "--3d is not in this build yet (2D force-graph only); the 3d-force-graph asset \
             lands in a follow-up. Use the default 2D view for now."
                .into(),
        );
    }

    let data = super::viz::viz_json_for(path, anchor, mode, depth)?;
    let html = VIEW_HTML
        .replace("/*FORCE_GRAPH_LIB*/", FORCE_GRAPH_JS)
        .replace("/*EDITOR*/", &editor_template(editor))
        .replace("/*ADEN_DATA*/", &data);

    let out_path: PathBuf = match out {
        Some(p) => p.to_path_buf(),
        None => std::env::temp_dir().join("aden-view.html"),
    };
    std::fs::write(&out_path, &html)?;
    println!("Wrote {}", out_path.display());

    if open {
        match open_in_browser(&out_path) {
            Ok(()) => println!("Opened in your default browser."),
            Err(e) => println!("Could not auto-open ({e}); open the file above manually."),
        }
    }
    Ok(())
}

/// Map an editor alias (or a custom `{file}`/`{line}` URI template) to the template
/// the viewer uses for "open in editor" links. Browsers route the registered custom
/// scheme straight to the desktop app, so no server is involved.
fn editor_template(editor: &str) -> String {
    match editor {
        "vscode" | "code" => "vscode://file{file}:{line}",
        "vscodium" | "codium" => "vscodium://file{file}:{line}",
        "cursor" => "cursor://file{file}:{line}",
        "zed" => "zed://file{file}:{line}",
        "idea" | "jetbrains" => "idea://open?file={file}&line={line}",
        custom if custom.contains("{file}") => custom,
        _ => "vscode://file{file}:{line}",
    }
    .to_string()
}

/// Best-effort: open a path in the OS default browser. Never blocks; a missing
/// opener (headless/CI) is reported, not fatal — `--no-open` is the explicit path.
fn open_in_browser(p: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let target = p.to_string_lossy().to_string();
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(&target);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", &target]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&target);
        c
    };
    cmd.spawn()?;
    Ok(())
}
