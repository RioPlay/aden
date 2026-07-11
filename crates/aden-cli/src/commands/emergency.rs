// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::path::Path;

pub fn cmd_emergency(
    path: &Path,
    reason: &str,
    ttl: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ttl_seconds = match ttl {
        "1h" => 3600,
        "24h" => 86400,
        "7d" => 604800,
        _ => return Err(format!("Invalid TTL '{}': use 1h, 24h, or 7d", ttl).into()),
    };

    let aden_dir = path.join(".aden");
    if !aden_dir.exists() {
        std::fs::create_dir_all(&aden_dir)?;
    }

    let now_secs = crate::time_util::now_unix_secs();
    let expires_secs = now_secs + ttl_seconds as u64;
    let tag = format!(
        "emergency-{}",
        crate::time_util::unix_secs_to_compact(now_secs)
    );

    let audit_log_path = aden_dir.join("emergency-audit.log");
    let audit_entry = format!(
        "[{}] EMERGENCY OVERRIDE created: reason='{}', expires={}, tag={}\n",
        crate::time_util::unix_secs_to_rfc3339(now_secs),
        reason,
        crate::time_util::unix_secs_to_rfc3339(expires_secs),
        tag
    );

    let mut audit_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_log_path)?;
    use std::io::Write;
    audit_file.write_all(audit_entry.as_bytes())?;

    let emergency_path = aden_dir.join("emergency-overrides.adoc");
    let content = format!(
        "[[{}]]\n= Emergency Override\n\n[override#{}]\n----\nEmergency override: all Forbid directives downgraded to Warn.\nExpires: {}\nReason: {}\n----\n",
        tag,
        tag,
        crate::time_util::unix_secs_to_rfc3339(expires_secs),
        reason
    );

    std::fs::write(&emergency_path, content)?;

    println!("[{}] EMERGENCY OVERRIDE created", tag);
    println!("  Reason: {}", reason);
    println!(
        "  Expires: {}",
        crate::time_util::unix_secs_to_rfc3339(expires_secs)
    );
    println!("  File: {}", emergency_path.display());
    println!("  Audit: {}", audit_log_path.display());

    Ok(())
}
