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
/// Vendored 3d-force-graph UMD bundle (MIT; bundles three.js) — the `--3d`
/// orbital view. Same pinning/checksum discipline as the 2D library.
const FORCE_GRAPH_3D_JS: &str = include_str!("../../assets/3d-force-graph.min.js");
/// Self-contained page template with `/*FORCE_GRAPH_LIB*/` and `/*ADEN_DATA*/`
/// placeholders (string-replaced, not `format!`, to avoid brace conflicts).
const VIEW_HTML: &str = include_str!("../../assets/view.html");
/// The `--3d` orbital-brain template (same placeholders, same data contract).
const VIEW3D_HTML: &str = include_str!("../../assets/view3d.html");

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
    scope: Option<&str>,
    resolution: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    if threed && replay {
        return Err(
            "--3d and --replay don't combine: replay is a 2D analytical view; \
                    the 3D orbital view is for spatial orientation. Pick one."
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
        let base = super::viz::viz_json_for(path, anchor, mode, depth, scope, resolution)?;
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
    // The data is inlined into a <script> tag, so any `</script>` (or `</`) inside a
    // symbol name / doc string from the target codebase would otherwise terminate the
    // script element and allow HTML/script injection when viewing an untrusted repo.
    // `<\/` is an identical JSON escape for `/` (parses to the same value) but does not
    // match the HTML end-tag tokenizer — the standard JSON-in-<script> hardening.
    let data = data.replace("</", "<\\/");

    let out_path: PathBuf = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let project = crate::util::find_project_root(path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(slug)
                .unwrap_or_else(|| "project".to_string());
            let anchor_slug = anchor
                .map(|a| format!("-{}", slug(a.rsplit(['#', '/']).next().unwrap_or(a))))
                .unwrap_or_default();
            let dim = if threed { "-3d" } else { "" };
            std::env::temp_dir().join(format!("aden-view-{project}{anchor_slug}{dim}.html"))
        }
    };
    // The sibling dimension lives next to the primary (same stem, `-3d`
    // toggled) so the in-page `d` key can hop 2D ↔ 3D as a relative link.
    let sibling_path: PathBuf = {
        let stem = out_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("aden-view");
        let sib_name = match stem.strip_suffix("-3d") {
            Some(base) => format!("{base}.html"),
            None => format!("{stem}-3d.html"),
        };
        out_path.with_file_name(sib_name)
    };
    let render = |template: &str, lib: &str, sibling: &str| {
        template
            .replace("/*FORCE_GRAPH_LIB*/", lib)
            // Substitute ONLY the double-quoted `const EDITOR = "/*EDITOR*/"`
            // assignment — NOT the single-quoted `EDITOR.includes('/*EDITOR*/')`
            // guard in editorUrl(). A blunt global replace of the bare token would
            // rewrite the guard's needle to the template too, so `EDITOR.includes(
            // <template>)` (EDITOR === <template>) is always true and every
            // open-in-editor link is suppressed. Targeting the quoted form leaves
            // the guard intact: it still fires only when the placeholder survives.
            .replace(
                "\"/*EDITOR*/\"",
                &format!("\"{}\"", editor_template(editor)),
            )
            .replace("/*SIBLING*/", sibling)
            .replace("/*ADEN_DATA*/", &data)
    };
    let sib_href = sibling_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out_href = out_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (tpl, lib, sib_tpl, sib_lib) = if threed {
        (VIEW3D_HTML, FORCE_GRAPH_3D_JS, VIEW_HTML, FORCE_GRAPH_JS)
    } else {
        (VIEW_HTML, FORCE_GRAPH_JS, VIEW3D_HTML, FORCE_GRAPH_3D_JS)
    };
    // Replay frames are a 2D-only surface; no sibling there (an empty
    // placeholder hides the `d` key in the page).
    std::fs::write(
        &out_path,
        render(tpl, lib, if replay { "" } else { sib_href.as_str() }),
    )?;
    if !replay {
        std::fs::write(&sibling_path, render(sib_tpl, sib_lib, &out_href))?;
        println!(
            "Wrote {} (+ sibling {})",
            out_path.display(),
            sibling_path.display()
        );
    } else {
        println!("Wrote {}", out_path.display());
    }

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
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let max_s = max.to_string();
    let mut args = vec![
        "log",
        "--reverse",
        "--no-merges",
        "--format=%H%x1f%s%x1f%cs",
    ];
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
        "auto" => return detect_editor_template(),
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

/// Pick the URI scheme of an editor that is actually installed, so the default
/// "open in editor" link lands somewhere. A hardcoded `vscode://` default goes
/// nowhere on machines running only VSCodium/Cursor/Zed (no handler for the
/// scheme registers, so clicks are silently dropped by the OS). Probed in
/// VS-Code-first order via PATH lookup — deterministic per machine; an explicit
/// `--editor` always wins by never reaching this.
fn detect_editor_template() -> String {
    for (bin, alias) in [
        ("code", "vscode"),
        ("codium", "codium"),
        ("cursor", "cursor"),
        ("zed", "zed"),
        ("idea", "idea"),
    ] {
        if binary_on_path(bin) {
            return editor_template(alias);
        }
    }
    editor_template("vscode")
}

/// Sanitise a raw name into a safe, readable filename segment: lowercase, runs of
/// non-alphanumeric chars collapsed to a single `-`, leading/trailing `-` stripped.
fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("project");
    }
    out
}

/// True if `bin` resolves to an executable on PATH (the same probe a shell does).
fn binary_on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let p = dir.join(bin);
        let exe = if cfg!(windows) {
            dir.join(format!("{bin}.exe")).is_file()
        } else {
            false
        };
        exe || p.is_file()
    })
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
