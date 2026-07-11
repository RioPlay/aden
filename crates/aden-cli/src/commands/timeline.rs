// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aden timeline` — bake every historical version of ONE file into a
//! self-contained HTML page.  Client-side JS handles the comparison so the
//! output works from `file://` with no server.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

struct Version {
    hash: String,
    short: String,
    date: String,
    subject: String,
    /// `None` means binary content — diff is not available.
    content: Option<String>,
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// `aden timeline <PATH> [--from …] [--to …] [--at …] [--out …] [--no-open] [--max N]`
///
/// Parameters mirror the subcommand flags 1:1.
pub fn cmd_timeline(
    path_arg: &str,
    from_ref: Option<&str>,
    to_ref: Option<&str>,
    at_ref: Option<&str>,
    max: usize,
    out: Option<&Path>,
    open: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // ------------------------------------------------------------------
    // 1. Resolve the file path
    // ------------------------------------------------------------------
    let relpath = resolve_relpath(path_arg)?;

    // ------------------------------------------------------------------
    // 2. Collect git history
    // ------------------------------------------------------------------
    let repo_root = crate::util::find_project_root(std::path::Path::new("."));
    let all_commits = git_log(&repo_root, &relpath)?;

    if all_commits.is_empty() {
        return Err(format!("File not tracked in git history: {relpath}").into());
    }

    // Apply --from / --to / --at ref filtering on the raw list.
    // The list from git log is newest-first; we work on that order for slicing,
    // then reverse to oldest→newest for display.
    let filtered = filter_by_refs(all_commits, from_ref, to_ref, at_ref);

    let total = filtered.len();
    // Cap to the `max` most-recent commits (still newest-first here).
    let (capped, shown) = if max > 0 && filtered.len() > max {
        (filtered[..max].to_vec(), max)
    } else {
        let len = filtered.len();
        (filtered, len)
    };

    // Collect per-commit content, oldest → newest.
    let mut versions: Vec<Version> = capped
        .into_iter()
        .rev() // oldest first
        .map(|(hash, date, subject)| {
            let content = git_show_content(&repo_root, &hash, &relpath);
            let short = if hash == "working-tree" {
                "now".to_string()
            } else {
                hash.chars().take(7).collect()
            };
            Version {
                hash,
                short,
                date,
                subject,
                content,
            }
        })
        .collect();

    // ------------------------------------------------------------------
    // 3. Append working-tree version
    // ------------------------------------------------------------------
    let abs_path = repo_root.join(&relpath);
    let wt_content = read_working_tree(&abs_path);
    let today = current_date();
    versions.push(Version {
        hash: "working-tree".to_string(),
        short: "now".to_string(),
        date: today,
        subject: "working tree".to_string(),
        content: wt_content,
    });

    // ------------------------------------------------------------------
    // 4. Generate HTML
    // ------------------------------------------------------------------
    let html = render_html(&relpath, &versions, shown, total);
    // JSON-in-<script> hardening: `<\/` prevents the HTML tokeniser from
    // seeing `</script>` inside the inline data block (same as view.rs).
    let html = html.replace("</", "<\\/");

    // ------------------------------------------------------------------
    // 5. Write output
    // ------------------------------------------------------------------
    let out_path: PathBuf = match out {
        Some(p) => p.to_path_buf(),
        None => {
            let slug = path_slug(&relpath);
            std::env::temp_dir().join(format!("aden-timeline-{slug}.html"))
        }
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

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Resolve `path_arg` (plain path or `aden://` anchor URI) to a repo-relative
/// string suitable for `git log -- <relpath>` and `git show <hash>:<relpath>`.
fn resolve_relpath(path_arg: &str) -> Result<String, Box<dyn std::error::Error>> {
    let raw = if let Some(rest) = path_arg.strip_prefix("aden://") {
        // Format: `module/PROJECT/REL/PATH#SYMBOL`
        // Strip "module/", skip project segment, join remaining up to '#'.
        let without_scheme = rest; // e.g. "module/myproject/src/lib.rs#foo"
        let no_hash = without_scheme.split('#').next().unwrap_or(without_scheme);
        let mut parts = no_hash.splitn(3, '/');
        let _module = parts.next(); // "module"
        let _project = parts.next(); // project name
        let relpath = parts.next().unwrap_or("").to_string();
        if relpath.is_empty() {
            return Err(format!("Could not extract file path from aden:// URI: {path_arg}").into());
        }
        relpath
    } else {
        path_arg.to_string()
    };

    // Normalize to repo-relative.
    let repo_root = crate::util::find_project_root(std::path::Path::new("."));
    let path = std::path::Path::new(&raw);
    let rel = if path.is_absolute() {
        path.strip_prefix(&repo_root)
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|_| {
                format!(
                    "Path '{}' is outside the repository root '{}'",
                    raw,
                    repo_root.display()
                )
            })?
    } else {
        // Relative to CWD; canonicalize then strip.
        let cwd = std::env::current_dir()?;
        let joined = cwd.join(&raw);
        // Use a manual normalize (no canonicalize — file may not exist on disk
        // in all historical revisions).
        let normalized = normalize_path(&joined);
        normalized
            .strip_prefix(&repo_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| raw.clone())
    };

    // Normalize separators to `/` (Windows compatibility).
    Ok(rel.replace('\\', "/"))
}

/// Lexically normalize a path (collapse `..` and `.`) without touching the
/// filesystem.  Returns the input unchanged if it contains no `.` components.
fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Git operations
// ---------------------------------------------------------------------------

/// One row of `git log` for a file: `(hash, date, subject)`.
type CommitRow = (String, String, String);

/// Run `git log --follow --pretty=format:%H%x09%ad%x09%s --date=short -- <relpath>`
/// and return `(hash, date, subject)` tuples, newest-first.
fn git_log(root: &Path, relpath: &str) -> Result<Vec<CommitRow>, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("git")
        .args([
            "log",
            "--follow",
            "--pretty=format:%H\t%ad\t%s",
            "--date=short",
            "--",
            relpath,
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("Not a git repository or git failed: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Not a git repository or git failed: {stderr}").into());
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    for line in stdout.lines() {
        let mut it = line.splitn(3, '\t');
        let hash = it.next().unwrap_or("").trim().to_string();
        let date = it.next().unwrap_or("").trim().to_string();
        let subject = it.next().unwrap_or("").trim().to_string();
        if !hash.is_empty() {
            entries.push((hash, date, subject));
        }
    }
    Ok(entries)
}

/// Return the file content at `hash:relpath`, or `None` if binary / missing.
fn git_show_content(root: &Path, hash: &str, relpath: &str) -> Option<String> {
    let spec = format!("{hash}:{relpath}");
    let out = std::process::Command::new("git")
        .args(["show", &spec])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // Binary detection: null byte in content.
    if out.stdout.contains(&b'\0') {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Read the working-tree version of the file.  Returns `None` if binary or
/// unreadable.
fn read_working_tree(abs_path: &Path) -> Option<String> {
    let bytes = std::fs::read(abs_path).ok()?;
    if bytes.contains(&b'\0') {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// Ref filtering
// ---------------------------------------------------------------------------

/// Keep only commits that fall within the `[from_ref, to_ref]` range, or a
/// single commit at `at_ref`.  When no filter is given the full list is
/// returned.  The list is assumed newest-first (as returned by `git log`).
fn filter_by_refs(
    commits: Vec<(String, String, String)>,
    from_ref: Option<&str>,
    to_ref: Option<&str>,
    at_ref: Option<&str>,
) -> Vec<(String, String, String)> {
    if let Some(at) = at_ref {
        let short = &at[..at.len().min(7)];
        return commits
            .into_iter()
            .filter(|(h, _, _)| h.starts_with(short) || h.starts_with(at))
            .collect();
    }
    if from_ref.is_none() && to_ref.is_none() {
        return commits;
    }

    // Find boundary indices (newest = index 0).
    let find_idx = |r: &str| -> Option<usize> {
        let short = &r[..r.len().min(7)];
        commits
            .iter()
            .position(|(h, _, _)| h.starts_with(short) || h.starts_with(r))
    };

    let to_idx = to_ref.and_then(find_idx).unwrap_or(0);
    let from_idx = from_ref
        .and_then(find_idx)
        .unwrap_or(commits.len().saturating_sub(1));

    // `to_ref` is newer (lower index), `from_ref` is older (higher index).
    let lo = to_idx.min(from_idx);
    let hi = to_idx.max(from_idx);
    commits[lo..=hi.min(commits.len().saturating_sub(1))].to_vec()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn current_date() -> String {
    // Use std only — no chrono dep.  Read from the system clock via
    // UNIX_EPOCH and manual decomposition.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Gregorian date from UNIX timestamp (seconds since 1970-01-01).
    unix_secs_to_date(secs)
}

/// Convert Unix seconds to `YYYY-MM-DD` without any external dependency.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm
/// (<http://howardhinnant.github.io/date_algorithms.html>), which maps a count
/// of days since the Unix epoch (1970-01-01) to a proleptic Gregorian
/// (year, month, day) triple.  All arithmetic is unsigned-safe for any date
/// representable in a `u64` seconds counter.
fn unix_secs_to_date(secs: u64) -> String {
    // Signed day number relative to the Unix epoch; safe cast because secs fits
    // in i64 for all dates within the realistic range of this tool.
    let z = (secs / 86_400) as i64 + 719_468_i64; // shift to 2000-03-01 epoch
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month of year starting from March [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Sanitize a file path into a safe filename stem.
fn path_slug(relpath: &str) -> String {
    let mut out = String::with_capacity(relpath.len());
    let mut last_dash = true;
    for c in relpath.chars() {
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
        out.push_str("file");
    }
    // Cap at 64 chars to stay within filesystem limits.
    out.truncate(64);
    out
}

/// Open `p` in the OS default browser.
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

// ---------------------------------------------------------------------------
// HTML generation
// ---------------------------------------------------------------------------

/// Render the complete self-contained HTML page.
fn render_html(relpath: &str, versions: &[Version], shown: usize, total: usize) -> String {
    let versions_json = build_versions_json(versions);
    let cap_note = if shown < total {
        format!(
            " <span class=\"cap-note\">(showing latest {} of {})</span>",
            shown, total
        )
    } else {
        String::new()
    };
    let version_count = versions.len();

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>aden timeline — {relpath}</title>
<style>
*, *::before, *::after {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  background: #0d0d0d;
  color: #cdd6f4;
  font-family: ui-sans-serif, system-ui, -apple-system, sans-serif;
  font-size: 14px;
  line-height: 1.5;
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}}
a {{ color: #89b4fa; text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
/* ── Header ── */
.header {{
  padding: 10px 16px;
  border-bottom: 1px solid #313244;
  background: rgba(17,21,30,.9);
  display: flex;
  align-items: baseline;
  gap: 10px;
  flex-shrink: 0;
}}
.header-path {{
  font-family: ui-monospace, 'Cascadia Code', 'Fira Code', monospace;
  font-size: 13px;
  color: #b4befe;
  font-weight: 600;
}}
.header-count {{
  font-size: 12px;
  color: #6c7086;
}}
.cap-note {{ color: #f38ba8; font-size: 11px; }}
/* ── Timeline strip ── */
.timeline-strip {{
  display: flex;
  gap: 6px;
  padding: 8px 16px;
  overflow-x: auto;
  border-bottom: 1px solid #313244;
  background: rgba(17,21,30,.6);
  flex-shrink: 0;
  scrollbar-width: thin;
  scrollbar-color: #313244 transparent;
}}
.chip {{
  flex-shrink: 0;
  padding: 3px 8px;
  border-radius: 4px;
  border: 1px solid #313244;
  background: #1e1e2e;
  font-size: 11px;
  color: #6c7086;
  cursor: pointer;
  white-space: nowrap;
  transition: border-color 0.15s, color 0.15s;
}}
.chip:hover {{ border-color: #89b4fa; color: #cdd6f4; }}
.chip.selected-base {{ border-color: #89b4fa; color: #89b4fa; background: rgba(137,180,250,0.12); }}
.chip.selected-compare {{ border-color: #b4befe; color: #b4befe; background: rgba(180,190,254,0.12); }}
/* ── Controls bar ── */
.controls {{
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 16px;
  border-bottom: 1px solid #313244;
  background: rgba(17,21,30,.8);
  flex-shrink: 0;
  flex-wrap: wrap;
}}
.ctrl-group {{
  display: flex;
  align-items: center;
  gap: 6px;
}}
.ctrl-label {{
  font-size: 11px;
  color: #6c7086;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}}
select {{
  background: #1e1e2e;
  color: #cdd6f4;
  border: 1px solid #313244;
  border-radius: 4px;
  padding: 4px 8px;
  font-size: 12px;
  font-family: ui-monospace, 'Cascadia Code', 'Fira Code', monospace;
  cursor: pointer;
  max-width: 260px;
}}
select:focus {{ outline: none; border-color: #89b4fa; }}
button {{
  background: #1e1e2e;
  color: #cdd6f4;
  border: 1px solid #313244;
  border-radius: 4px;
  padding: 4px 10px;
  font-size: 12px;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}}
button:hover {{ border-color: #89b4fa; color: #89b4fa; }}
.spacer {{ flex: 1; }}
/* ── Version info bar ── */
.version-info {{
  display: flex;
  gap: 20px;
  padding: 6px 16px;
  border-bottom: 1px solid #313244;
  background: rgba(17,21,30,.7);
  font-size: 12px;
  flex-shrink: 0;
}}
.vi-block {{ display: flex; flex-direction: column; gap: 1px; }}
.vi-label {{ font-size: 10px; color: #6c7086; text-transform: uppercase; letter-spacing: 0.05em; }}
.vi-value {{ font-family: ui-monospace, 'Cascadia Code', 'Fira Code', monospace; color: #cdd6f4; }}
.vi-base .vi-label {{ color: #89b4fa; }}
.vi-compare .vi-label {{ color: #b4befe; }}
/* ── Diff area ── */
.diff-container {{
  flex: 1;
  display: grid;
  grid-template-columns: 1fr 1fr;
  overflow: hidden;
  gap: 0;
}}
.diff-pane {{
  overflow: auto;
  border-right: 1px solid #313244;
  scrollbar-width: thin;
  scrollbar-color: #313244 transparent;
}}
.diff-pane:last-child {{ border-right: none; }}
.diff-table {{
  width: 100%;
  border-collapse: collapse;
  font-family: ui-monospace, 'Cascadia Code', 'Fira Code', monospace;
  font-size: 12px;
  white-space: pre;
}}
.diff-table td {{
  padding: 0 4px;
  vertical-align: top;
  border: none;
}}
.ln {{ color: #45475a; text-align: right; user-select: none; min-width: 36px; border-right: 1px solid #313244; padding-right: 6px; }}
.code {{ padding-left: 8px; width: 100%; }}
.add {{ background: rgba(166,227,161,0.15); }}
.del {{ background: rgba(243,139,168,0.15); }}
.binary-note {{
  padding: 24px 20px;
  color: #6c7086;
  font-style: italic;
  font-size: 13px;
}}
.no-diff {{
  padding: 24px 20px;
  color: #6c7086;
  font-size: 13px;
}}
</style>
</head>
<body>
<div class="header">
  <span class="header-path">{relpath}</span>
  <span class="header-count">{version_count} versions{cap_note}</span>
</div>
<div class="timeline-strip" id="strip"></div>
<div class="controls">
  <div class="ctrl-group">
    <span class="ctrl-label">Base</span>
    <select id="selBase"></select>
  </div>
  <div class="ctrl-group">
    <span class="ctrl-label">Compare</span>
    <select id="selCompare"></select>
  </div>
  <button id="btnToday" title="Set Compare to working tree">Compare to today</button>
  <button id="btnPrev" title="Step both selectors one commit back">← adjacent</button>
  <button id="btnNext" title="Step both selectors one commit forward">→ adjacent</button>
  <span class="spacer"></span>
</div>
<div class="version-info" id="versionInfo"></div>
<div class="diff-container">
  <div class="diff-pane" id="paneLeft"></div>
  <div class="diff-pane" id="paneRight"></div>
</div>
<script>
(function() {{
'use strict';

const VERSIONS = {versions_json};

// ── Selectors ────────────────────────────────────────────────────────────
const strip = document.getElementById('strip');
const selBase = document.getElementById('selBase');
const selCompare = document.getElementById('selCompare');
const btnToday = document.getElementById('btnToday');
const btnPrev = document.getElementById('btnPrev');
const btnNext = document.getElementById('btnNext');
const versionInfo = document.getElementById('versionInfo');
const paneLeft = document.getElementById('paneLeft');
const paneRight = document.getElementById('paneRight');

// ── Populate dropdowns & chips ───────────────────────────────────────────
VERSIONS.forEach(function(v, i) {{
  const label = v.date + ' · ' + v.short + (v.subject ? ' · ' + v.subject : '');
  const opt = function(el) {{
    const o = document.createElement('option');
    o.value = i;
    o.textContent = label;
    el.appendChild(o);
  }};
  opt(selBase);
  opt(selCompare);

  const chip = document.createElement('div');
  chip.className = 'chip';
  chip.textContent = label;
  chip.dataset.idx = i;
  chip.addEventListener('click', function() {{
    const idx = parseInt(this.dataset.idx);
    const bi = parseInt(selBase.value);
    const ci = parseInt(selCompare.value);
    // First click sets base, second (different) click sets compare.
    if (bi === ci || idx === bi) {{
      selBase.value = idx;
    }} else {{
      selCompare.value = idx;
    }}
    refresh();
  }});
  strip.appendChild(chip);
}});

// Default: oldest as base, last (working-tree) as compare.
selBase.value = 0;
selCompare.value = VERSIONS.length - 1;

// ── Button handlers ──────────────────────────────────────────────────────
btnToday.addEventListener('click', function() {{
  selCompare.value = VERSIONS.length - 1;
  refresh();
}});

btnPrev.addEventListener('click', function() {{
  const bi = parseInt(selBase.value);
  const ci = parseInt(selCompare.value);
  if (bi > 0 && ci > 0) {{
    selBase.value = bi - 1;
    selCompare.value = ci - 1;
    refresh();
  }}
}});

btnNext.addEventListener('click', function() {{
  const bi = parseInt(selBase.value);
  const ci = parseInt(selCompare.value);
  if (bi < VERSIONS.length - 1 && ci < VERSIONS.length - 1) {{
    selBase.value = parseInt(bi) + 1;
    selCompare.value = parseInt(ci) + 1;
    refresh();
  }}
}});

selBase.addEventListener('change', refresh);
selCompare.addEventListener('change', refresh);

// ── LCS diff ─────────────────────────────────────────────────────────────
function lcs(a, b) {{
  const m = a.length, n = b.length;
  const dp = Array.from({{length: m+1}}, function() {{ return new Int32Array(n+1); }});
  for (let i = 1; i <= m; i++)
    for (let j = 1; j <= n; j++)
      dp[i][j] = a[i-1] === b[j-1] ? dp[i-1][j-1]+1 : Math.max(dp[i-1][j], dp[i][j-1]);
  const result = [];
  let i = m, j = n;
  while (i > 0 || j > 0) {{
    if (i > 0 && j > 0 && a[i-1] === b[j-1]) {{ result.push(['=', a[i-1]]); i--; j--; }}
    else if (j > 0 && (i === 0 || dp[i][j-1] >= dp[i-1][j])) {{ result.push(['+', b[j-1]]); j--; }}
    else {{ result.push(['-', a[i-1]]); i--; }}
  }}
  return result.reverse();
}}

// ── Render diff ───────────────────────────────────────────────────────────
function escHtml(s) {{
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}}

function renderDiff(baseV, cmpV) {{
  if (baseV.content === null || cmpV.content === null) {{
    const note = '<div class="binary-note">[binary file — diff not available]<\/div>';
    paneLeft.innerHTML = note;
    paneRight.innerHTML = note;
    return;
  }}
  if (baseV.content === cmpV.content) {{
    const note = '<div class="no-diff">Files are identical.<\/div>';
    paneLeft.innerHTML = note;
    paneRight.innerHTML = note;
    return;
  }}
  const aLines = baseV.content.split('\n');
  const bLines = cmpV.content.split('\n');

  // LCS is O(m*n) in time and space — guard very large files.
  const MAX_LINES = 4000;
  if (aLines.length > MAX_LINES || bLines.length > MAX_LINES) {{
    const lNote = buildTable(aLines.map(function(l) {{ return ['=', l]; }}), 'left');
    const rNote = buildTable(bLines.map(function(l) {{ return ['=', l]; }}), 'right');
    paneLeft.innerHTML = '<div class="no-diff">File too large for diff (' + aLines.length + ' lines); showing raw.<\/div>' + lNote;
    paneRight.innerHTML = '<div class="no-diff">File too large for diff (' + bLines.length + ' lines); showing raw.<\/div>' + rNote;
    return;
  }}

  const ops = lcs(aLines, bLines);

  // Split ops into left (base) and right (compare) sides.
  const leftOps = ops.filter(function(op) {{ return op[0] === '=' || op[0] === '-'; }});
  const rightOps = ops.filter(function(op) {{ return op[0] === '=' || op[0] === '+'; }});

  paneLeft.innerHTML = buildTable(leftOps, 'left');
  paneRight.innerHTML = buildTable(rightOps, 'right');

  // Synchronize scroll positions.
  syncScroll(paneLeft, paneRight);
}}

function buildTable(ops, side) {{
  const rows = [];
  let ln = 1;
  for (let k = 0; k < ops.length; k++) {{
    const op = ops[k][0];
    const text = ops[k][1];
    let cls = '';
    if (op === '+') cls = ' class="add"';
    else if (op === '-') cls = ' class="del"';
    const prefix = op === '=' ? ' ' : op;
    rows.push('<tr' + cls + '>' +
      '<td class="ln">' + ln + '<\/td>' +
      '<td class="code">' + escHtml(prefix + text) + '<\/td>' +
      '<\/tr>');
    ln++;
  }}
  return '<table class="diff-table"><tbody>' + rows.join('') + '<\/tbody><\/table>';
}}

let _syncLock = false;
function syncScroll(a, b) {{
  function handler(src, dst) {{
    return function() {{
      if (_syncLock) return;
      _syncLock = true;
      dst.scrollTop = src.scrollTop;
      _syncLock = false;
    }};
  }}
  a.removeEventListener('scroll', a._syncHandler);
  b.removeEventListener('scroll', b._syncHandler);
  a._syncHandler = handler(a, b);
  b._syncHandler = handler(b, a);
  a.addEventListener('scroll', a._syncHandler);
  b.addEventListener('scroll', b._syncHandler);
}}

// ── Info bar & chip highlight ─────────────────────────────────────────────
function updateInfoBar(bi, ci) {{
  const bv = VERSIONS[bi];
  const cv = VERSIONS[ci];
  versionInfo.innerHTML =
    '<div class="vi-block vi-base">' +
      '<span class="vi-label">Base<\/span>' +
      '<span class="vi-value">' + escHtml(bv.date + ' · ' + bv.short + ' · ' + bv.subject) + '<\/span>' +
    '<\/div>' +
    '<div class="vi-block vi-compare">' +
      '<span class="vi-label">Compare<\/span>' +
      '<span class="vi-value">' + escHtml(cv.date + ' · ' + cv.short + ' · ' + cv.subject) + '<\/span>' +
    '<\/div>';

  Array.from(strip.querySelectorAll('.chip')).forEach(function(chip) {{
    const idx = parseInt(chip.dataset.idx);
    chip.classList.remove('selected-base', 'selected-compare');
    if (idx === bi) chip.classList.add('selected-base');
    if (idx === ci) chip.classList.add('selected-compare');
  }});
}}

// ── Main refresh ──────────────────────────────────────────────────────────
function refresh() {{
  const bi = parseInt(selBase.value);
  const ci = parseInt(selCompare.value);
  updateInfoBar(bi, ci);
  renderDiff(VERSIONS[bi], VERSIONS[ci]);
}}

// Initial render.
refresh();

}})();
<\/script>
</body>
</html>"#,
        relpath = escape_html_attr(relpath),
        cap_note = cap_note,
        version_count = version_count,
        versions_json = versions_json,
    )
}

/// Build the `VERSIONS` JSON literal.  All strings are JSON-encoded so the
/// inline `<script>` block is always syntactically valid.
fn build_versions_json(versions: &[Version]) -> String {
    let mut parts = Vec::with_capacity(versions.len());
    for v in versions {
        let hash_json = json_string(&v.hash);
        let short_json = json_string(&v.short);
        let date_json = json_string(&v.date);
        let subject_json = json_string(&v.subject);
        let content_json = match &v.content {
            Some(c) => json_string(c),
            None => "null".to_string(),
        };
        parts.push(format!(
            r#"{{"hash":{hash_json},"short":{short_json},"date":{date_json},"subject":{subject_json},"content":{content_json}}}"#
        ));
    }
    format!("[{}]", parts.join(","))
}

/// Minimal JSON string encoder: wraps in `"`, escaping the six mandatory
/// JSON escapes plus non-ASCII as `\uXXXX`.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str(r#"\""#),
            '\\' => out.push_str(r"\\"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            '\x08' => out.push_str(r"\b"),
            '\x0c' => out.push_str(r"\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escape a string for use as HTML attribute content (inside `"…"`).
fn escape_html_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_basic() {
        assert_eq!(path_slug("src/lib.rs"), "src-lib-rs");
        assert_eq!(path_slug(""), "file");
    }

    #[test]
    fn json_string_escapes_specials() {
        let s = json_string("hello\nworld\"tab\t");
        assert_eq!(s, r#""hello\nworld\"tab\t""#);
    }

    #[test]
    fn json_string_handles_backslash() {
        let s = json_string(r"a\b");
        assert_eq!(s, r#""a\\b""#);
    }

    #[test]
    fn unix_secs_to_date_epoch() {
        // 1970-01-01
        assert_eq!(unix_secs_to_date(0), "1970-01-01");
    }

    #[test]
    fn unix_secs_to_date_known() {
        // 2024-01-15 = 1705276800
        assert_eq!(unix_secs_to_date(1_705_276_800), "2024-01-15");
    }

    #[test]
    fn resolve_aden_uri() {
        let result = resolve_relpath("aden://module/myproject/src/lib.rs#foo");
        assert!(result.is_ok(), "should parse aden:// uri: {result:?}");
        // The result depends on the repo root, but the extracted part must end with src/lib.rs.
        let relpath = result.unwrap();
        assert!(
            relpath.ends_with("src/lib.rs"),
            "expected …src/lib.rs, got {relpath}"
        );
    }

    #[test]
    fn normalize_path_collapses_dots() {
        let p = PathBuf::from("/a/b/../c/./d");
        assert_eq!(normalize_path(&p), PathBuf::from("/a/c/d"));
    }

    #[test]
    fn filter_no_refs_returns_all() {
        let commits = vec![
            ("aaa".to_string(), "2024-01-03".to_string(), "c".to_string()),
            ("bbb".to_string(), "2024-01-02".to_string(), "b".to_string()),
            ("ccc".to_string(), "2024-01-01".to_string(), "a".to_string()),
        ];
        let result = filter_by_refs(commits.clone(), None, None, None);
        assert_eq!(result, commits);
    }

    #[test]
    fn filter_at_ref_selects_single() {
        let commits = vec![
            (
                "abc1234".to_string(),
                "2024-01-03".to_string(),
                "c".to_string(),
            ),
            (
                "def5678".to_string(),
                "2024-01-02".to_string(),
                "b".to_string(),
            ),
        ];
        let result = filter_by_refs(commits, None, None, Some("def5678"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "def5678");
    }

    #[test]
    fn build_versions_json_null_binary() {
        let versions = vec![Version {
            hash: "abc".to_string(),
            short: "abc".to_string(),
            date: "2024-01-01".to_string(),
            subject: "init".to_string(),
            content: None,
        }];
        let json = build_versions_json(&versions);
        assert!(
            json.contains("\"content\":null"),
            "binary content must serialize as null: {json}"
        );
    }
}
