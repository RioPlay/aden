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
    replay: bool,
    max: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if threed {
        return Err(
            "--3d is not in this build yet (2D force-graph only); the 3d-force-graph asset \
             lands in a follow-up. Use the default 2D view for now."
                .into(),
        );
    }

    // A bare `aden view` (no anchor, default mode) opens the whole-graph view — the full
    // importance-ranked graph, which also carries the git-history activity log so replay
    // walks the entire project populating piece by piece. (Was the communities overview;
    // the whole-graph view is the richer canonical surface.)
    let mode = if !replay && anchor.is_none() && mode == "blast" {
        "graph"
    } else {
        mode
    };
    // Emit a bit deeper than asked so the viewer's depth slider has range to dial.
    let depth = if replay { depth } else { depth.max(3) };
    // `--replay`: the graph IS the project's git-history surface — the union of all
    // symbols touched across `max` commits — and `DATA.activity` carries each commit's
    // touched anchors so the viewer plays the project *populating* over time.
    // Otherwise it's the normal `viz` slice.
    let data = if replay {
        let root = crate::util::find_project_root(path);
        let activity = git_activity(&root, max);
        let touched: std::collections::BTreeSet<String> = activity
            .iter()
            .filter_map(|f| f["anchors"].as_array())
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        let base = super::viz::anchors_json(path, &touched, 250)?;
        match serde_json::from_str::<serde_json::Value>(&base) {
            Ok(mut v) => {
                v["activity"] = serde_json::Value::Array(activity);
                serde_json::to_string_pretty(&v).unwrap_or(base)
            }
            Err(_) => base,
        }
    } else {
        let base = super::viz::viz_json_for(path, anchor, mode, depth)?;
        // The whole-graph view is also the canonical *replay* surface: attach the full
        // git-history activity log so the viewer can play the entire project populating,
        // piece by piece, across every commit — over the real 800-node graph rather than
        // a synthetic walk. (Touched anchors not in the importance cap simply don't light;
        // the lens reveals the kept graph in commit order.)
        if mode == "graph" {
            let root = crate::util::find_project_root(path);
            let activity = git_activity(&root, max);
            if !activity.is_empty() {
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&base) {
                    v["activity"] = serde_json::Value::Array(activity);
                    serde_json::to_string_pretty(&v).unwrap_or(base)
                } else {
                    base
                }
            } else {
                base
            }
        } else {
            base
        }
    };
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

/// Build a per-commit activity log from git history (oldest → newest): each frame is
/// `{label: subject, anchors: [symbols in the files that commit changed]}`. Symbols
/// resolve via the store's per-file spans (same source as "open in editor"). The
/// viewer plays these back, pulsing the touched nodes — the project being built.
fn git_activity(root: &Path, max: usize) -> Vec<serde_json::Value> {
    use std::collections::{BTreeSet, HashMap};
    // file (repo-relative) → anchors defined in it
    let mut file_anchors: HashMap<String, Vec<String>> = HashMap::new();
    for (file, spans) in super::grep::load_symbol_spans(root) {
        let e = file_anchors.entry(file).or_default();
        for sp in spans {
            e.push(sp.anchor);
        }
    }
    let run = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let max_s = max.to_string();
    let mut args = vec!["log", "--reverse", "--no-merges", "--format=%H%x1f%s%x1f%cs"];
    if max > 0 {
        args.push("-n");
        args.push(&max_s); // `--max 0` → entire history
    }
    let Some(log) = run(&args) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in log.lines() {
        let mut it = line.splitn(3, '\u{1f}');
        let hash = it.next().unwrap_or("");
        let subject = it.next().unwrap_or("");
        let date = it.next().unwrap_or("");
        if hash.is_empty() {
            continue;
        }
        let Some(files) = run(&["show", "--name-only", "--pretty=format:", hash]) else {
            continue;
        };
        let mut anchors: BTreeSet<String> = BTreeSet::new();
        for f in files.lines().map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(list) = file_anchors.get(f) {
                anchors.extend(list.iter().cloned());
            }
        }
        if anchors.is_empty() {
            continue;
        }
        out.push(serde_json::json!({
            "label": subject,
            "date": date,
            "anchors": anchors.into_iter().collect::<Vec<_>>(),
        }));
    }
    out
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
