use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Workspace-relative path of the helper that clones upstream grammar sources.
const CLONE_VENDORS_SCRIPT: &str = "scripts/clone_vendors.py";

/// Env var naming an explicit interpreter for [`CLONE_VENDORS_SCRIPT`].
const CLONE_VENDORS_INTERPRETER_ENV: &str = "TSLP_PYTHON";

/// Interpreter probe list used when [`CLONE_VENDORS_INTERPRETER_ENV`] is unset.
const DEFAULT_CLONE_VENDORS_RUNNERS: &[&[&str]] = &[
    &["uv", "run", "--no-sync"],
    &["uv", "run"],
    &["python3"],
    &["python"],
];

/// Probe whether the parsers tree at `root` looks populated for the requested
/// selection. Uses the same heuristic in both the workspace and OUT_DIR cache
/// locations.
fn parsers_root_populated(root: &Path, selected: &[String]) -> bool {
    match selected.first() {
        Some(first) => root.join(first).join("src/parser.c").exists(),
        None => root.join("python").join("src/parser.c").exists(),
    }
}

/// Try to populate the workspace `parsers/` tree by invoking
/// `scripts/clone_vendors.py` from `project_root`. Returns true if the script
/// ran successfully AND the tree is populated afterwards. Returns false if no
/// script is present, no runner is available, or the script failed.
///
/// This is the local-development fallback: a fresh workspace clone has
/// `parsers/` gitignored, and a rc whose release hasn't been published yet has
/// no GH tarball — so neither the workspace nor the remote path works. Cloning
/// upstream grammars via the script bridges the gap.
fn try_clone_vendors_locally(project_root: &Path, parsers_dir: &Path, selected: &[String]) -> bool {
    let clone_script = project_root.join(CLONE_VENDORS_SCRIPT);
    if !clone_script.exists() {
        return false;
    }

    println!("cargo:rerun-if-env-changed={CLONE_VENDORS_INTERPRETER_ENV}");
    println!(
        "cargo:warning=parsers/ tree is empty; running {CLONE_VENDORS_SCRIPT} to populate from upstream grammars"
    );

    let runners = clone_vendors_runners();
    let mut spawned_any = false;

    for cmd_args in &runners {
        let mut cmd = std::process::Command::new(&cmd_args[0]);
        cmd.args(&cmd_args[1..]);
        cmd.current_dir(project_root);
        match cmd.status() {
            Ok(status) if status.success() => {
                if parsers_root_populated(parsers_dir, selected) {
                    return true;
                }
                println!(
                    "cargo:warning={} succeeded but parsers/ is still not populated",
                    cmd_args.join(" ")
                );
                return false;
            }
            Ok(status) => {
                spawned_any = true;
                println!(
                    "cargo:warning={} exited with {:?}",
                    cmd_args.join(" "),
                    status.code()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!(
                    "cargo:warning=interpreter '{}' not found on PATH",
                    cmd_args[0]
                );
            }
            Err(e) => {
                spawned_any = true;
                println!("cargo:warning=could not run {}: {e}", cmd_args.join(" "));
            }
        }
    }

    if !spawned_any {
        report_no_interpreter(&runners);
    }
    false
}

/// Build the ordered list of argv vectors used to run [`CLONE_VENDORS_SCRIPT`].
///
/// A non-empty [`CLONE_VENDORS_INTERPRETER_ENV`] replaces the probe list entirely
/// so hermetic builds (nix, containers, vendored toolchains) do not depend on
/// `uv` or `python3` happening to be on `PATH`.
fn clone_vendors_runners() -> Vec<Vec<String>> {
    let script = CLONE_VENDORS_SCRIPT.to_string();

    if let Ok(interpreter) = env::var(CLONE_VENDORS_INTERPRETER_ENV)
        && !interpreter.trim().is_empty()
    {
        return vec![vec![interpreter.trim().to_string(), script]];
    }

    DEFAULT_CLONE_VENDORS_RUNNERS
        .iter()
        .map(|base| {
            base.iter()
                .map(|a| (*a).to_string())
                .chain([script.clone()])
                .collect()
        })
        .collect()
}

/// Emit an actionable diagnostic when no candidate interpreter could be spawned.
fn report_no_interpreter(runners: &[Vec<String>]) {
    let mut tried: Vec<&str> = Vec::new();
    for runner in runners {
        let name = runner[0].as_str();
        if !tried.contains(&name) {
            tried.push(name);
        }
    }
    println!(
        "cargo:warning=No usable Python interpreter was found to run {CLONE_VENDORS_SCRIPT} (tried: {}). \
         Install `uv` or `python3`, or set {CLONE_VENDORS_INTERPRETER_ENV} to an interpreter path. \
         Falling back to the released parser-source bundle; set TSLP_OFFLINE=1 to skip parser sources entirely.",
        tried.join(", ")
    );
}

/// Resolve grammar sources. Workspace, offline, and local-clone sources are
/// external (`None` owner); downloaded sources return their OUT_DIR scratch
/// root as `Some` so build.rs can remove it after embedding.
pub(crate) fn ensure_parser_sources(
    parsers_dir: &Path,
    selected: &[String],
    out_dir: &Path,
) -> (PathBuf, Option<PathBuf>) {
    println!("cargo:rerun-if-env-changed=TSLP_OFFLINE");
    println!("cargo:rerun-if-env-changed=TSLP_SOURCE_BUNDLE_URL");

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    let default_url = format!(
        "https://github.com/xberg-io/tree-sitter-language-pack/releases/download/v{version}/parser-sources-{version}.tar.zst"
    );
    let url = env::var("TSLP_SOURCE_BUNDLE_URL").unwrap_or(default_url);
    assert!(
        !url.contains(['\n', '\r']),
        "TSLP_SOURCE_BUNDLE_URL cannot contain newlines"
    );
    let sha_url = format!("{url}.sha256");
    track_local_bundle_file(&url);
    track_local_bundle_file(&sha_url);

    if parsers_root_populated(parsers_dir, selected) {
        return (parsers_dir.to_path_buf(), None);
    }
    if env::var("TSLP_OFFLINE").is_ok_and(|v| !v.is_empty() && v != "0") {
        println!("cargo:warning=TSLP_OFFLINE set; refusing to download parser sources");
        return (parsers_dir.to_path_buf(), None);
    }

    // ~keep Dev checkouts clone upstream grammars before falling back to release tarballs.
    // ~keep This covers fresh workspaces and unpublished rc builds.
    let project_root = parsers_dir.parent().unwrap_or(parsers_dir).to_path_buf();
    if try_clone_vendors_locally(&project_root, parsers_dir, selected) {
        return (parsers_dir.to_path_buf(), None);
    }

    let body = fetch_bytes(&url).unwrap_or_else(|e| {
        panic!(
            "Failed to download parser sources from {url}: {e}. Set TSLP_OFFLINE=1 to skip, or ensure network access and that v{version} is published with a parser-sources-{version}.tar.zst asset."
        )
    });
    // ~keep The bytes fetched above become C that is compiled into this binary, so a
    // missing or malformed checksum sidecar is a hard error rather than a skipped
    // check. TSLP_OFFLINE=1 is the existing escape hatch for "do not download at all".
    let sha_text = fetch_text(&sha_url).unwrap_or_else(|e| {
        panic!(
            "Failed to download the SHA-256 sidecar {sha_url}: {e}. The parser-source bundle is compiled into this crate, so it is refused without an integrity check. Set TSLP_OFFLINE=1 to skip downloading parser sources entirely, or point TSLP_SOURCE_BUNDLE_URL at a bundle that has a .sha256 sidecar next to it."
        )
    });
    let expected = sha_text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        expected.len() == 64 && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "Malformed SHA-256 sidecar at {sha_url}: expected a 64-character hex digest, got {expected:?}. Refusing to compile unverified parser sources; set TSLP_OFFLINE=1 to skip the download entirely."
    );
    let digest = Sha256::digest(&body);
    let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        expected, actual,
        "SHA-256 mismatch for {url}: expected {expected}, got {actual}"
    );

    let scratch_dir = prepare_bundle_scratch(out_dir);
    let cursor = std::io::Cursor::new(body);
    let extraction = (|| -> Result<(), String> {
        let decoder = zstd::stream::read::Decoder::with_buffer(cursor)
            .map_err(|e| format!("create zstd decoder: {e}"))?;
        let mut archive = tar::Archive::new(decoder);
        unpack_selected_bundle_members(&mut archive, &scratch_dir, selected)?;
        Ok(())
    })();
    if let Err(error) = extraction {
        remove_bundle_scratch_if_safe(&scratch_dir);
        panic!("Failed to extract parser-source bundle from {url}: {error}");
    }

    let downloaded_parsers = scratch_dir.join("parsers");
    if !downloaded_parsers.is_dir() {
        remove_bundle_scratch_if_safe(&scratch_dir);
        panic!("Parser-source bundle from {url} contains no parser directories");
    }
    (downloaded_parsers, Some(scratch_dir))
}

fn track_local_bundle_file(url: &str) {
    let Some(path) = url.strip_prefix("file://") else {
        return;
    };
    assert!(
        !path.contains(['\n', '\r']),
        "TSLP_SOURCE_BUNDLE_URL file paths cannot contain newlines"
    );
    println!("cargo:rerun-if-changed={path}");
}

fn prepare_bundle_scratch(out_dir: &Path) -> PathBuf {
    fs::create_dir_all(out_dir).expect("create Cargo OUT_DIR");
    let scratch_dir = out_dir.join("_aden_bundle_inputs");
    match fs::symlink_metadata(&scratch_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            panic!(
                "Refusing to follow scratch symlink {}",
                scratch_dir.display()
            );
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&scratch_dir).unwrap_or_else(|e| {
                panic!(
                    "remove stale parser bundle scratch {}: {e}",
                    scratch_dir.display()
                )
            });
        }
        Ok(_) => panic!(
            "Parser bundle scratch path is not a directory: {}",
            scratch_dir.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "inspect parser bundle scratch {}: {error}",
            scratch_dir.display()
        ),
    }
    fs::create_dir(&scratch_dir).unwrap_or_else(|e| {
        panic!(
            "create parser bundle scratch {}: {e}",
            scratch_dir.display()
        )
    });
    scratch_dir
}

fn remove_bundle_scratch_if_safe(scratch_dir: &Path) {
    if let Ok(metadata) = fs::symlink_metadata(scratch_dir)
        && metadata.is_dir()
        && !metadata.file_type().is_symlink()
    {
        let _ = fs::remove_dir_all(scratch_dir);
    }
}

fn unpack_selected_bundle_members<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    scratch_dir: &Path,
    selected: &[String],
) -> Result<(), String> {
    for item in archive
        .entries()
        .map_err(|e| format!("read tar entries: {e}"))?
    {
        let mut entry = item.map_err(|e| format!("read tar entry: {e}"))?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err("unsupported tar member type".to_string());
        }
        let path_bytes = entry.path_bytes();
        let raw_path =
            std::str::from_utf8(&path_bytes).map_err(|e| format!("tar path is not UTF-8: {e}"))?;
        let safe_path = safe_archive_path(raw_path, entry_type.is_dir())?;
        if safe_path.is_empty() {
            continue;
        }
        let Some(parser_root_member) =
            selected_bundle_member(safe_path, selected, entry_type.is_dir())
        else {
            continue;
        };
        let destination = if parser_root_member {
            let parser_root = scratch_dir.join("parsers");
            fs::create_dir_all(&parser_root)
                .map_err(|e| format!("create {}: {e}", parser_root.display()))?;
            parser_root
        } else {
            scratch_dir.to_path_buf()
        };
        drop(path_bytes);
        match entry.unpack_in(&destination) {
            Ok(true) => {}
            Ok(false) => return Err("tar refused a selected member".to_string()),
            Err(e) => return Err(format!("unpack selected tar member: {e}")),
        }
    }
    Ok(())
}

fn safe_archive_path(path: &str, is_dir: bool) -> Result<&str, String> {
    let had_leading_dot = path.starts_with("./");
    let mut path = path;
    while let Some(without_dot) = path.strip_prefix("./") {
        path = without_dot;
    }
    if (path == "." || path.is_empty() && had_leading_dot) && is_dir {
        return Ok("");
    }
    if path.ends_with('/') {
        if !is_dir {
            return Err(format!("file tar path ends in slash: {path:?}"));
        }
        path = path.strip_suffix('/').unwrap_or(path);
    }
    if path.is_empty() || path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        return Err(format!("unsafe tar path {path:?}"));
    }
    for component in path.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
            || component
                .chars()
                .any(|c| c.is_control() || "<>\"|?*".contains(c))
            || is_windows_device_name(component)
        {
            return Err(format!("unsafe tar path {path:?}"));
        }
    }
    Ok(path)
}

fn is_windows_device_name(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or_default();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|name| base.eq_ignore_ascii_case(name))
    {
        return true;
    }
    let Some(prefix) = base.get(..3) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case("COM") && !prefix.eq_ignore_ascii_case("LPT") {
        return false;
    }
    matches!(
        base.get(3..),
        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
    )
}

/// Return whether this member should be unpacked. `true` means the member is
/// a flat-layout grammar path and should be rooted under scratch/parsers;
/// `false` means it already has its final path relative to scratch.
fn selected_bundle_member(path: &str, selected: &[String], is_dir: bool) -> Option<bool> {
    let mut components = path.split('/');
    let first = components.next()?;
    if matches!(first, "patches" | "sources" | "query-overlays") {
        return Some(false);
    }
    if first == "parsers" {
        let Some(language) = components.next() else {
            return Some(false);
        };
        let child = components.next();
        if selected.iter().any(|name| name == language) || child.is_none() && is_dir {
            return Some(false);
        }
        if child == Some("queries") {
            return Some(false);
        }
        return None;
    }

    let child = components.next();
    if child.is_none() {
        return is_dir.then_some(true);
    }
    if selected.iter().any(|name| name == first) || child == Some("queries") {
        return Some(true);
    }
    None
}

fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        return fs::read(path).map_err(|e| format!("read {path}: {e}"));
    }

    // ~keep Retry transport errors, including intermittent GitHub release CDN 504s during publish verification.
    let max_attempts = 6u32;
    let mut last_err = String::new();
    for attempt in 1..=max_attempts {
        let call_result = ureq::get(url)
            .config()
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .call();

        match call_result {
            Ok(response) => {
                let mut reader = response.into_body().into_reader();
                let mut buf = Vec::new();
                match std::io::Read::read_to_end(&mut reader, &mut buf) {
                    Ok(_) => return Ok(buf),
                    Err(e) => last_err = format!("read body for {url}: {e}"),
                }
            }
            Err(e) => last_err = format!("GET {url}: {e}"),
        }

        if attempt < max_attempts {
            let backoff = Duration::from_secs(2u64.saturating_pow(attempt));
            println!(
                "cargo:warning={last_err}; retrying in {}s (attempt {}/{})",
                backoff.as_secs(),
                attempt + 1,
                max_attempts
            );
            std::thread::sleep(backoff);
        }
    }
    Err(last_err)
}

fn fetch_text(url: &str) -> Result<String, String> {
    let bytes = fetch_bytes(url)?;
    String::from_utf8(bytes).map_err(|e| format!("decode utf8: {e}"))
}
