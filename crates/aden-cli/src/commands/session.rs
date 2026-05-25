use std::path::Path;

use crate::util::{escape_adoc_cell, safe_relative, validate_name};

/// Interactive kickoff wizard. Fills the kickoff template via Q&A.
pub fn cmd_kickoff(
    name: &str,
    interactive: bool,
    repo: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_name(name)?;
    use std::io::{self, Write};

    let kickoff_template = include_str!("../../../../.agent/templates/kickoff.adoc");
    let out_path = repo.join("docs").join(format!("kickoff-{}.adoc", name));
    std::fs::create_dir_all(out_path.parent().unwrap_or(Path::new(".")))?;

    if interactive {
        println!("=== Aden Kickoff Wizard ===");
        println!("Answer a few questions to scaffold the kickoff document.\n");

        let q = |prompt: &str| -> Result<String, Box<dyn std::error::Error>> {
            print!("{}", prompt);
            io::stdout().flush()?;
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            Ok(buf.trim().to_string())
        };

        let problem = q("What problem does this solve? ")?;
        let who = q("Who has this problem? ")?;
        let _success = q("What does success look like? ")?;
        let _non_goal = q("What is explicitly NOT in scope? ")?;
        let _deadline = q("Deadline (or 'TBD')? ")?;
        let owner = q("Primary owner? ")?;

        let resolved = kickoff_template
            .replace("{project}", name)
            .replace(
                "{date}",
                aden_core::rfc3339_now()
                    .split('T')
                    .next()
                    .unwrap_or("2026-01-01"),
            )
            .replace("{author}", &owner)
            .replace("{idea}", &name.to_lowercase().replace(" ", "-"));

        // Replace template placeholders with guided content
        let mut output = resolved;
        // Replace first blank line in Problem section
        output = output.replace(
            ". Who has this problem?\n. ",
            &format!(". Who has this problem?\n  *Answer:* {}\n. ", who),
        );
        output = output.replace(
            ". What do they do today without your solution?\n",
            &format!(
                ". What do they do today without your solution?\n  *Answer:* {}\n",
                problem
            ),
        );

        std::fs::write(&out_path, output)?;
        println!("\n✓ Generated {}", out_path.display());
        println!("  Review and edit before proceeding to `aden workflow design`.");
    } else {
        // Non-interactive: just fill placeholders from template
        let resolved = kickoff_template
            .replace("{project}", name)
            .replace(
                "{date}",
                aden_core::rfc3339_now()
                    .split('T')
                    .next()
                    .unwrap_or("2026-01-01"),
            )
            .replace("{author}", "<author>")
            .replace("{idea}", &name.to_lowercase().replace(" ", "-"));
        std::fs::write(&out_path, resolved)?;
        println!("Generated kickoff template: {}", out_path.display());
        println!("  Fill in the blank sections, then run:");
        println!("    aden workflow design --from {}", out_path.display());
    }

    Ok(())
}

/// Workflow engine: instantiate a template from a source document.
pub fn cmd_workflow(
    template: &str,
    from: Option<&str>,
    out: Option<&Path>,
    repo: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let templates: std::collections::HashMap<&str, &str> = [
        (
            "design",
            include_str!("../../../../.agent/templates/design.adoc"),
        ),
        (
            "spec",
            include_str!("../../../../.agent/templates/spec.adoc"),
        ),
        (
            "task",
            include_str!("../../../../.agent/templates/task.adoc"),
        ),
        ("adr", include_str!("../../../../.agent/templates/adr.adoc")),
        (
            "kickoff",
            include_str!("../../../../.agent/templates/kickoff.adoc"),
        ),
        (
            "plan",
            include_str!("../../../../.agent/templates/plan.adoc"),
        ),
        (
            "context",
            include_str!("../../../../.agent/templates/context.adoc"),
        ),
        (
            "module",
            include_str!("../../../../.agent/templates/module.adoc"),
        ),
        (
            "runbook",
            include_str!("../../../../.agent/templates/runbook.adoc"),
        ),
        (
            "glossary",
            include_str!("../../../../.agent/templates/glossary.adoc"),
        ),
        (
            "constraints",
            include_str!("../../../../.agent/templates/constraints.adoc"),
        ),
    ]
    .into_iter()
    .collect();

    let tmpl = templates.get(template).ok_or_else(|| {
        format!(
            "Unknown template '{}'. Supported: {}",
            template,
            templates.keys().copied().collect::<Vec<_>>().join(", ")
        )
    })?;

    // Resolve placeholders from source doc if --from is given
    let mut resolved = tmpl.to_string();
    if let Some(src_path_str) = from {
        safe_relative(src_path_str)?;
        let src_path = repo.join(src_path_str);
        if src_path.exists() {
            let src_text = std::fs::read_to_string(&src_path)?;
            // Extract key-value pairs from AsciiDoc attributes
            for line in src_text.lines() {
                if line.starts_with(':')
                    && line.contains(": ")
                    && let Some((key, value)) = line.trim().split_once(": ")
                {
                    let key = key.trim_start_matches(':');
                    let placeholder = format!("{{{key}}}");
                    resolved = resolved.replace(&placeholder, value.trim());
                }
            }
            // Extract anchor as {feature}/{idea} if present
            if let Some(anchor) = src_text.lines().find(|l| l.starts_with("[[")) {
                let inner = anchor.trim_start_matches("[[").trim_end_matches("]]");
                let clean = inner.replace(['{', '}'], "");
                resolved = resolved.replace("{feature}", &clean);
                resolved = resolved.replace("{idea}", &clean);
            }
        }
    }

    // Default values for any remaining placeholders
    let now = aden_core::rfc3339_now()
        .split('T')
        .next()
        .unwrap_or("2026-01-01")
        .to_string();
    resolved = resolved.replace("{date}", &now);
    resolved = resolved.replace("{author}", "<author>");
    resolved = resolved.replace(
        "{project_name}",
        repo.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
    );
    resolved = resolved.replace(
        "{project}",
        repo.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
    );
    resolved = resolved.replace("{feature}", "feature-name");
    resolved = resolved.replace("{idea}", "idea-name");
    resolved = resolved.replace("{number}", "001");
    resolved = resolved.replace("{phase}", "0");
    resolved = resolved.replace("{standard}", "unknown");
    resolved = resolved.replace("{lang}", "unknown");
    resolved = resolved.replace("{ai_name}", "agent");
    resolved = resolved.replace("{primary_lang}", "unknown");
    resolved = resolved.replace("{framework}", "unknown");
    resolved = resolved.replace("{edition}", "2024");
    resolved = resolved.replace("{glossary}", "(fill me in)");
    resolved = resolved.replace("{dependencies}", "(fill me in)");

    // Auto-next step suggestion
    let next_hint = match template {
        "kickoff" => Some("aden workflow design --from docs/kickoff-<name>.adoc"),
        "design" => Some("aden workflow adr --from docs/design-<name>.adoc"),
        "adr" => Some("aden workflow spec --from docs/design-<name>.adoc"),
        "spec" => Some("aden workflow task --from docs/spec-<name>.adoc"),
        "task" => Some("start implementing, then run: aden gen src/"),
        _ => None,
    };

    if let Some(out_path) = out {
        safe_relative(&out_path.to_string_lossy())?;
    }
    let dest = if let Some(out_path) = out {
        out_path.to_path_buf()
    } else {
        let safe = template.to_lowercase().replace(" ", "-");
        repo.join("docs").join(format!("{}-unnamed.adoc", safe))
    };

    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(&dest, resolved)?;
    println!("✓ Generated workflow document: {}", dest.display());
    if let Some(hint) = next_hint {
        println!("  Next step: {}", hint);
    }

    Ok(())
}

/// Atomic session lock: append entry to .agent/session.adoc.
pub fn cmd_session(
    repo_path: &Path,
    agent_id: &str,
    task: &str,
    files: Option<&str>,
    status: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate inputs against injection and length attacks
    const MAX_FIELD_LEN: usize = 500;
    if agent_id.len() > MAX_FIELD_LEN || task.len() > MAX_FIELD_LEN || status.len() > MAX_FIELD_LEN
    {
        return Err("Input field exceeds maximum length (500 chars)".into());
    }
    if files.map(|f| f.len() > MAX_FIELD_LEN).unwrap_or(false) {
        return Err("Files field exceeds maximum length (500 chars)".into());
    }

    let session_path = repo_path.join(".agent").join("session.adoc");

    if !session_path.exists() {
        return Err(format!(
            "Session file not found: {}. Run 'aden init' first.",
            session_path.display()
        )
        .into());
    }

    // Enforce session file size limit to prevent DoS via log growth
    const MAX_SESSION_SIZE: u64 = 5 * 1024 * 1024; // 5 MB
    let meta = std::fs::metadata(&session_path)?;
    if meta.len() > MAX_SESSION_SIZE {
        return Err("Session log exceeds 5 MB. Rotate or archive before appending.".into());
    }

    let timestamp = aden_core::rfc3339_now();
    let files_str = files.unwrap_or("-");
    let entry = format!(
        "|{} |{} |{} |{} |{}\n",
        escape_adoc_cell(&timestamp),
        escape_adoc_cell(agent_id),
        escape_adoc_cell(task),
        escape_adoc_cell(files_str),
        escape_adoc_cell(status)
    );

    let mut content = std::fs::read_to_string(&session_path)?;

    // Find the table body and append
    if let Some(pos) = content.find("|===\n\n== Known Invariants") {
        let insert_pos = pos; // Insert before "== Known Invariants"
        let before = &content[..insert_pos];
        let after = &content[insert_pos..];
        let new_content = format!("{}\n{}\n{}", before.trim_end(), entry, after);
        content = new_content;
    } else {
        // Fallback: append to end
        content.push('\n');
        content.push_str(&entry);
    }

    // Atomic write: temp file + rename to prevent race conditions between agents
    let temp_path = session_path.with_extension("tmp");
    std::fs::write(&temp_path, &content)?;
    std::fs::rename(&temp_path, &session_path)?;

    println!(
        "Session entry logged for agent '{}': {}",
        agent_id,
        session_path.display()
    );
    Ok(())
}
