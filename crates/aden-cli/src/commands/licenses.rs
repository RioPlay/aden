// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Polyglot third-party license accreditation.
//!
//! aden is a language-agnostic tool, so `aden licenses` must not be Rust-only.
//! It detects every dependency ecosystem present in the target repo, parses the
//! corresponding lockfile for `(name, version)`, and resolves each package's
//! license **local-first** (from the on-disk installed package) with a network
//! registry fallback when `--full` is requested and no local copy is found.
//!
//! Supported ecosystems mirror the manifests the rest of the CLI already detects
//! (see `misc.rs`): Cargo (Rust), npm (Node), PyPI (Python), Go.

use std::collections::BTreeSet;
#[cfg(feature = "licenses-net")]
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecosystem {
    Cargo,
    Npm,
    PyPI,
    Go,
}

impl Ecosystem {
    fn label(self) -> &'static str {
        match self {
            Ecosystem::Cargo => "Rust (Cargo)",
            Ecosystem::Npm => "Node (npm)",
            Ecosystem::PyPI => "Python (PyPI)",
            Ecosystem::Go => "Go (modules)",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Ecosystem::Cargo => "cargo",
            Ecosystem::Npm => "npm",
            Ecosystem::PyPI => "pypi",
            Ecosystem::Go => "go",
        }
    }
}

struct Package {
    name: String,
    version: String,
    ecosystem: Ecosystem,
}

#[derive(Default)]
struct LicenseInfo {
    license: Option<String>,
    repository: Option<String>,
    /// Where the metadata came from: "lockfile", "node_modules", "cargo cache",
    /// "site-packages", "crates.io", "npm", "pypi", "deps.dev", or "unknown".
    source: &'static str,
}

/// Detect which ecosystems have a lockfile/manifest in `repo`.
fn detect(repo: &Path) -> Vec<Ecosystem> {
    let mut found = Vec::new();
    if repo.join("Cargo.lock").exists() {
        found.push(Ecosystem::Cargo);
    }
    if repo.join("package-lock.json").exists() {
        found.push(Ecosystem::Npm);
    }
    if repo.join("poetry.lock").exists() || repo.join("requirements.txt").exists() {
        found.push(Ecosystem::PyPI);
    }
    if repo.join("go.sum").exists() {
        found.push(Ecosystem::Go);
    }
    found
}

// ── Lockfile parsers ────────────────────────────────────────────────────────

/// Parse a Cargo.lock (TOML `[[package]]` blocks). aden's own crates are skipped.
fn parse_cargo(repo: &Path) -> Vec<Package> {
    let content = match std::fs::read_to_string(repo.join("Cargo.lock")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut packages = Vec::new();
    let mut name: Option<String> = None;
    let mut is_aden = true;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("[[package]]") {
            is_aden = true;
            name = None;
        } else if let Some(rest) = t.strip_prefix("name = ") {
            let n = rest.trim_matches('"').to_string();
            is_aden = n.starts_with("aden");
            name = Some(n);
        } else if let Some(rest) = t.strip_prefix("version = ")
            && !is_aden
            && let Some(n) = name.clone()
        {
            packages.push(Package {
                name: n,
                version: rest.trim_matches('"').to_string(),
                ecosystem: Ecosystem::Cargo,
            });
        }
    }
    packages
}

/// Parse a package-lock.json. v2/v3 use the `packages` map keyed by install
/// path (`node_modules/foo`); v1 uses the nested `dependencies` tree.
fn parse_npm(repo: &Path) -> Vec<Package> {
    let content = match std::fs::read_to_string(repo.join("package-lock.json")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(j) => j,
        Err(_) => return Vec::new(),
    };

    let mut packages = Vec::new();
    if let Some(map) = json.get("packages").and_then(|p| p.as_object()) {
        for (key, val) in map {
            // The "" key is the root project itself, not a dependency.
            let Some(idx) = key.rfind("node_modules/") else {
                continue;
            };
            let name = &key[idx + "node_modules/".len()..];
            if name.is_empty() {
                continue;
            }
            let version = val
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            packages.push(Package {
                name: name.to_string(),
                version,
                ecosystem: Ecosystem::Npm,
            });
        }
    } else if let Some(deps) = json.get("dependencies").and_then(|d| d.as_object()) {
        // npm lockfile v1
        for (name, val) in deps {
            let version = val
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            packages.push(Package {
                name: name.clone(),
                version,
                ecosystem: Ecosystem::Npm,
            });
        }
    }
    packages
}

/// Parse Python deps: prefer poetry.lock (`[[package]]` TOML), else requirements.txt.
fn parse_python(repo: &Path) -> Vec<Package> {
    let poetry = repo.join("poetry.lock");
    if poetry.exists()
        && let Ok(content) = std::fs::read_to_string(&poetry)
    {
        let mut packages = Vec::new();
        let mut name: Option<String> = None;
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("[[package]]") {
                name = None;
            } else if let Some(rest) = t.strip_prefix("name = ") {
                name = Some(rest.trim_matches('"').to_string());
            } else if let Some(rest) = t.strip_prefix("version = ")
                && let Some(n) = name.take()
            {
                packages.push(Package {
                    name: n,
                    version: rest.trim_matches('"').to_string(),
                    ecosystem: Ecosystem::PyPI,
                });
            }
        }
        return packages;
    }

    // requirements.txt: pinned "name==version" lines only; skip unpinned, URLs,
    // options, markers, and comments — we can't attribute what isn't versioned.
    let mut packages = Vec::new();
    if let Ok(content) = std::fs::read_to_string(repo.join("requirements.txt")) {
        for line in content.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('-') {
                continue;
            }
            // Drop environment markers ("pkg==1.0 ; python_version<'3.8'").
            let spec = line.split(';').next().unwrap_or("").trim();
            if let Some((name, version)) = spec.split_once("==") {
                let name = name.split('[').next().unwrap_or("").trim();
                if !name.is_empty() {
                    packages.push(Package {
                        name: name.to_string(),
                        version: version.trim().to_string(),
                        ecosystem: Ecosystem::PyPI,
                    });
                }
            }
        }
    }
    packages
}

/// Parse go.sum: lines are `module version hash`, with a separate `/go.mod`
/// entry per module. Dedup on `(name, version)`.
fn parse_go(repo: &Path) -> Vec<Package> {
    let content = match std::fs::read_to_string(repo.join("go.sum")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut packages = Vec::new();
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(version)) = (parts.next(), parts.next()) else {
            continue;
        };
        // "v1.2.3/go.mod" and "v1.2.3" describe the same module version.
        let version = version.strip_suffix("/go.mod").unwrap_or(version);
        let key = (name.to_string(), version.to_string());
        if seen.insert(key) {
            packages.push(Package {
                name: name.to_string(),
                version: version.to_string(),
                ecosystem: Ecosystem::Go,
            });
        }
    }
    packages
}

fn parse(repo: &Path, eco: Ecosystem) -> Vec<Package> {
    let mut pkgs = match eco {
        Ecosystem::Cargo => parse_cargo(repo),
        Ecosystem::Npm => parse_npm(repo),
        Ecosystem::PyPI => parse_python(repo),
        Ecosystem::Go => parse_go(repo),
    };
    pkgs.sort_by(|a, b| a.name.cmp(&b.name).then(a.version.cmp(&b.version)));
    pkgs.dedup_by(|a, b| a.name == b.name && a.version == b.version);
    pkgs
}

// ── License resolution (local-first, network fallback) ──────────────────────

/// Extract `license`/`repository` from a parsed TOML table (Cargo.toml).
fn license_from_cargo_toml(content: &str) -> LicenseInfo {
    let mut info = LicenseInfo {
        source: "cargo cache",
        ..Default::default()
    };
    if let Ok(doc) = content.parse::<toml_edit::DocumentMut>()
        && let Some(pkg) = doc.get("package").and_then(|p| p.as_table())
    {
        info.license = pkg
            .get("license")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        info.repository = pkg
            .get("repository")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if info.license.is_none() && pkg.get("license-file").is_some() {
            info.license = Some("see license-file".to_string());
        }
    }
    info
}

/// Look for `<name>-<version>` in the local cargo registry cache.
fn local_cargo(name: &str, version: &str) -> Option<LicenseInfo> {
    let base = dirs::home_dir()?.join(".cargo/registry/src");
    let entries = std::fs::read_dir(&base).ok()?;
    let needle = format!("{name}-{version}");
    for entry in entries.flatten() {
        let manifest = entry.path().join(&needle).join("Cargo.toml");
        if let Ok(content) = std::fs::read_to_string(&manifest) {
            return Some(license_from_cargo_toml(&content));
        }
    }
    None
}

/// Read license/repository from `node_modules/<name>/package.json`.
fn local_npm(repo: &Path, name: &str) -> Option<LicenseInfo> {
    let pkg_json = repo.join("node_modules").join(name).join("package.json");
    let content = std::fs::read_to_string(&pkg_json).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let license = match json.get("license") {
        // Modern: a string SPDX id. Legacy: { "type": "...", "url": "..." }.
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(obj) => obj.get("type").and_then(|t| t.as_str()).map(str::to_string),
        None => json
            .get("licenses")
            .and_then(|l| l.as_array())
            .and_then(|a| a.first())
            .and_then(|f| f.get("type"))
            .and_then(|t| t.as_str())
            .map(str::to_string),
    };
    let repository = match json.get("repository") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(obj) => obj.get("url").and_then(|u| u.as_str()).map(str::to_string),
        None => None,
    };
    Some(LicenseInfo {
        license,
        repository,
        source: "node_modules",
    })
}

/// Find `<name>-<version>.dist-info/METADATA` in any site-packages under `repo`.
fn local_pypi(repo: &Path, name: &str, version: &str) -> Option<LicenseInfo> {
    // Distribution names normalize runs of -, _, . to a single separator; the
    // dist-info dir uses underscores. Match case-insensitively on the canon form.
    let canon = |s: &str| s.to_lowercase().replace(['-', '.'], "_");
    let want_dir = format!("{}-{version}.dist-info", canon(name));
    for venv in [".venv", "venv", "env"] {
        let lib = repo.join(venv).join("lib");
        let Ok(pyvers) = std::fs::read_dir(&lib) else {
            continue;
        };
        for py in pyvers.flatten() {
            let site = py.path().join("site-packages");
            let Ok(dists) = std::fs::read_dir(&site) else {
                continue;
            };
            for dist in dists.flatten() {
                if canon(&dist.file_name().to_string_lossy()) == want_dir
                    && let Ok(meta) = std::fs::read_to_string(dist.path().join("METADATA"))
                {
                    return Some(license_from_metadata(&meta));
                }
            }
        }
    }
    None
}

/// Parse a Python core-metadata METADATA file for its license.
fn license_from_metadata(meta: &str) -> LicenseInfo {
    let mut license = None;
    let mut repository = None;
    for line in meta.lines() {
        if line.is_empty() {
            break; // headers end at the first blank line
        }
        if let Some(rest) = line.strip_prefix("License-Expression:") {
            license = Some(rest.trim().to_string());
        } else if license.is_none()
            && let Some(rest) = line.strip_prefix("License:")
        {
            let v = rest.trim();
            if !v.is_empty() && v != "UNKNOWN" {
                license = Some(v.to_string());
            }
        } else if license.is_none()
            && let Some(rest) = line.strip_prefix("Classifier: License ::")
        {
            // "License :: OSI Approved :: MIT License" -> last segment.
            if let Some(last) = rest.rsplit("::").next() {
                license = Some(last.trim().to_string());
            }
        } else if repository.is_none()
            && let Some(rest) = line.strip_prefix("Project-URL:")
            && let Some((label, url)) = rest.split_once(',')
            && matches!(
                label.trim().to_lowercase().as_str(),
                "source" | "repository" | "homepage" | "source code"
            )
        {
            repository = Some(url.trim().to_string());
        }
    }
    LicenseInfo {
        license,
        repository,
        source: "site-packages",
    }
}

fn local_license(repo: &Path, pkg: &Package) -> Option<LicenseInfo> {
    let info = match pkg.ecosystem {
        Ecosystem::Cargo => local_cargo(&pkg.name, &pkg.version),
        Ecosystem::Npm => local_npm(repo, &pkg.name),
        Ecosystem::PyPI => local_pypi(repo, &pkg.name, &pkg.version),
        Ecosystem::Go => None, // module cache carries no license metadata
    }?;
    // A hit with no license string is no better than a miss for attribution.
    if info.license.is_some() {
        Some(info)
    } else {
        None
    }
}

#[cfg(feature = "licenses-net")]
fn http_json(url: &str) -> Option<serde_json::Value> {
    let resp = ureq::get(url)
        .header("User-Agent", "aden-licenses")
        .call()
        .ok()?;
    if resp.status() != 200 {
        return None;
    }
    let mut body = String::new();
    resp.into_body()
        .into_reader()
        .read_to_string(&mut body)
        .ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(feature = "licenses-net")]
fn network_license(pkg: &Package) -> LicenseInfo {
    match pkg.ecosystem {
        Ecosystem::Cargo => {
            let url = format!(
                "https://crates.io/api/v1/crates/{}/{}",
                pkg.name, pkg.version
            );
            if let Some(j) = http_json(&url).and_then(|j| j.get("version").cloned()) {
                return LicenseInfo {
                    license: j
                        .get("license")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    repository: j
                        .get("repository")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    source: "crates.io",
                };
            }
        }
        Ecosystem::Npm => {
            let url = format!("https://registry.npmjs.org/{}/{}", pkg.name, pkg.version);
            if let Some(j) = http_json(&url) {
                let license = j
                    .get("license")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let repository = j
                    .get("repository")
                    .and_then(|r| r.get("url").and_then(|u| u.as_str()))
                    .map(str::to_string);
                return LicenseInfo {
                    license,
                    repository,
                    source: "npm",
                };
            }
        }
        Ecosystem::PyPI => {
            let url = format!("https://pypi.org/pypi/{}/{}/json", pkg.name, pkg.version);
            if let Some(info) = http_json(&url).and_then(|j| j.get("info").cloned()) {
                let license = info
                    .get("license_expression")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        info.get("license")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty() && *s != "UNKNOWN")
                    })
                    .map(str::to_string);
                return LicenseInfo {
                    license,
                    repository: info
                        .get("home_page")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    source: "pypi",
                };
            }
        }
        Ecosystem::Go => {
            // deps.dev covers Go modules, which carry no license in the lockfile.
            let url = format!(
                "https://api.deps.dev/v3/systems/go/packages/{}/versions/{}",
                pkg.name, pkg.version
            );
            if let Some(j) = http_json(&url) {
                let license = j
                    .get("licenses")
                    .and_then(|l| l.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|s| !s.is_empty());
                return LicenseInfo {
                    license,
                    repository: None,
                    source: "deps.dev",
                };
            }
        }
    }
    LicenseInfo {
        source: "unknown",
        ..Default::default()
    }
}

/// Resolve a package's license. With `network`, fall back to the registry when
/// no local copy supplies one; otherwise report whatever the lockfile/local has.
fn resolve(repo: &Path, pkg: &Package, network: bool) -> LicenseInfo {
    if let Some(info) = local_license(repo, pkg) {
        return info;
    }
    #[cfg(feature = "licenses-net")]
    if network {
        return network_license(pkg);
    }
    // When licenses-net is not compiled in, the network flag has no effect.
    #[cfg(not(feature = "licenses-net"))]
    let _ = network;
    LicenseInfo {
        source: "unknown",
        ..Default::default()
    }
}

// ── Command entry point ──────────────────────────────────────────────────────

pub fn cmd_licenses(
    repo_path: &Path,
    out: Option<&Path>,
    full: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let ecosystems = detect(repo_path);
    if ecosystems.is_empty() {
        return Err(format!(
            "No supported lockfile found at {}. Looked for Cargo.lock, package-lock.json, \
             poetry.lock/requirements.txt, go.sum.",
            repo_path.display()
        )
        .into());
    }

    // (ecosystem, package, resolved license)
    let mut rows: Vec<(Ecosystem, Package, LicenseInfo)> = Vec::new();
    for eco in &ecosystems {
        for pkg in parse(repo_path, *eco) {
            // `full` enriches with license text (and may hit the network);
            // the plain table stays offline and fast.
            let info = if full || json {
                resolve(repo_path, &pkg, full)
            } else {
                LicenseInfo::default()
            };
            rows.push((*eco, pkg, info));
        }
    }

    if json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|(eco, pkg, info)| {
                serde_json::json!({
                    "ecosystem": eco.id(),
                    "name": pkg.name,
                    "version": pkg.version,
                    "license": info.license,
                    "repository": info.repository,
                    "source": info.source,
                })
            })
            .collect();
        let output = serde_json::to_string_pretty(&arr)?;
        return write_out(out, &output, "JSON license data");
    }

    let markdown = render_markdown(&ecosystems, &rows, full);
    write_out(out, &markdown, "third-party attribution")
}

fn render_markdown(
    ecosystems: &[Ecosystem],
    rows: &[(Ecosystem, Package, LicenseInfo)],
    full: bool,
) -> String {
    let mut md = String::new();
    md.push_str("# Third-Party Dependencies\n\n");
    md.push_str("This project uses the following open-source packages.\n");
    md.push_str("Generated by `aden licenses`.\n\n");
    md.push_str(&format!(
        "Detected ecosystems: {}.\n\n",
        ecosystems
            .iter()
            .map(|e| e.label())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let mut license_counts: std::collections::BTreeMap<String, usize> = Default::default();

    for eco in ecosystems {
        let eco_rows: Vec<_> = rows.iter().filter(|(e, _, _)| e == eco).collect();
        if eco_rows.is_empty() {
            continue;
        }
        md.push_str(&format!(
            "## {} — {} packages\n\n",
            eco.label(),
            eco_rows.len()
        ));

        if full {
            for (_, pkg, info) in &eco_rows {
                let license = info.license.as_deref().unwrap_or("UNKNOWN");
                *license_counts.entry(license.to_string()).or_insert(0) += 1;
                // Print the version verbatim — Go module versions already carry
                // their `v` prefix (`v1.9.1`); Cargo/npm/PyPI do not. Hardcoding
                // `v` here double-prefixed Go (`vv1.9.1`). Matches the plain table.
                md.push_str(&format!("### {} {}\n\n", pkg.name, pkg.version));
                md.push_str(&format!("- **License**: {license}\n"));
                if let Some(repo) = &info.repository {
                    md.push_str(&format!("- **Repository**: {repo}\n"));
                }
                md.push_str(&format!("- **Source**: {}\n\n", info.source));
            }
        } else {
            md.push_str("| Package | Version |\n|---------|---------|\n");
            for (_, pkg, _) in &eco_rows {
                md.push_str(&format!("| {} | {} |\n", pkg.name, pkg.version));
            }
            md.push('\n');
        }
    }

    if full && !license_counts.is_empty() {
        md.push_str("## License Summary\n\n| License | Count |\n|--------|-------|\n");
        let mut sorted: Vec<_> = license_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (license, count) in sorted {
            md.push_str(&format!("| {license} | {count} |\n"));
        }
        md.push('\n');
    }

    md.push_str("## Attribution\n\n");
    md.push_str(
        "All third-party packages are used in accordance with their respective licenses.\n",
    );
    md.push_str("No proprietary code is bundled or modified without explicit permission.\n");
    md.push_str("\n---\nGenerated by Aden.\n");
    md
}

fn write_out(
    out: Option<&Path>,
    content: &str,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(path) = out {
        std::fs::write(path, content)?;
        println!("Wrote {what} to {}", path.display());
    } else {
        println!("{content}");
    }
    Ok(())
}
