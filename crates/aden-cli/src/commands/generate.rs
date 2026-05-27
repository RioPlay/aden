use std::path::{Path, PathBuf};

use crate::types::GenCacheEntry;
use crate::util::{
    base_cache_path, discover_source_files, emit_docs, find_project_root, load_gen_cache,
    sanitize_anchor, sanitize_source_file, save_gen_cache,
};

/// Automatically generate module contracts for directories in the workspace.
/// This ensures deterministic module anchors exist before symbol contracts.
/// Language-agnostic: works for any project with src/ directories.
fn generate_module_contracts(root: &Path, out_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Common source directory names across languages
    let src_dirs = ["src", "lib", "app", "modules", "source"];
    
    // Find all directories that could be modules (contain source files)
    let mut modules: Vec<(String, PathBuf)> = Vec::new();
    
    // Check for common workspace structures
    let workspace_dirs = [
        root.join("crates"),
        root.join("packages"),
        root.join("modules"),
        root.join("src"),
    ];
    
    for ws_dir in workspace_dirs.iter() {
        if !ws_dir.is_dir() {
            continue;
        }
        
        for entry in std::fs::read_dir(ws_dir)? {
            let entry = entry?;
            let mod_path = entry.path();
            if !mod_path.is_dir() {
                continue;
            }
            
            // Check if this module has source files
            for src_name in &src_dirs {
                let src_path = mod_path.join(src_name);
                if src_path.is_dir() {
                    let mod_name = mod_path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !mod_name.is_empty() && !mod_name.starts_with('.') {
                        modules.push((mod_name.to_string(), src_path));
                    }
                    break;
                }
            }
        }
    }
    
    // Also check root src/ directly
    for src_name in &src_dirs {
        let src_path = root.join(src_name);
        if src_path.is_dir() {
            let mod_name = root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string();
            modules.push((mod_name, src_path));
            break;
        }
    }
    
    // Generate module contracts
    for (mod_name, src_path) in modules {
        let module_anchor = format!("mod-{}", mod_name);
        let contract_file = format!("{}.adoc", module_anchor);
        
        let out_path = out_dir.join(&contract_file);

        // Skip if already exists and valid
        if out_path.exists() {
            if let Ok(existing) = std::fs::read_to_string(&out_path) {
                if existing.contains(&format!("[[{}]]", module_anchor)) {
                    continue;
                }
            }
        }

        let content = format!(
            r#":source_file: {src}
:node-type: module
:last-verified: {date}T00:00:00Z

[[{anchor}]]
= {name}

Part of: <<mod-project>>
"#,
            src = src_path.display(),
            date = chrono::Utc::now().format("%Y-%m-%d"),
            anchor = module_anchor,
            name = mod_name
        );

        // Also create the root project module if it doesn't exist
        let project_anchor = "mod-project";
        let project_file = format!("{}.adoc", project_anchor);
        let project_path = out_dir.join(&project_file);
        if !project_path.exists() {
            let project_content = format!(
                r#":source_file: .
:node-type: module
:last-verified: {}T00:00:00Z

[[mod-project]]
= Project Root

Root module for the project. All submodules reference this.

== Modules

"#,
                chrono::Utc::now().format("%Y-%m-%d")
            );
            std::fs::write(&project_path, &project_content)?;
            println!("Generated: {}", project_path.display());
        }

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &content)?;
        println!("Generated module: {} ({})", out_path.display(), src_path.display());
    }

    Ok(())
}

/// Detect existing contract structure and return matching output directory.
/// Supports any workspace layout: crates/, packages/, modules/, or flat src/.
fn detect_contract_structure(path: &Path, source_path: &Path) -> Option<std::path::PathBuf> {
    let root = find_project_root(path);
    let rel = source_path.strip_prefix(&root).ok()?;

    let components: Vec<_> = rel.components().collect();
    if components.len() >= 3 {
        let first = components[0].as_os_str().to_str()?;
        let second = components[1].as_os_str().to_str()?;

        // Support any named workspace directory, not just "crates"
        for workspace_dir in &["crates", "packages", "modules"] {
            if first == *workspace_dir {
                let contract_dir = root
                    .join("contracts")
                    .join(workspace_dir)
                    .join(second)
                    .join("src");
                if contract_dir.exists() && contract_dir.is_dir() {
                    return Some(contract_dir);
                }
            }
        }
    }

    None
}

/// Infer parent module anchor from source file path.
/// Works with any directory layout: looks for the first named sub-directory
/// under known workspace roots (crates/, packages/, modules/, src/), or
/// falls back to the immediate parent directory of the source file.
fn infer_parent_module_from_source(source_path: &Path) -> Option<String> {
    let path_str = source_path.to_string_lossy();

    // Check for common workspace sub-directory patterns
    for workspace_dir in &["/crates/", "/packages/", "/modules/"] {
        if let Some(start) = path_str.find(workspace_dir) {
            let after = &path_str[start + workspace_dir.len()..];
            if let Some(end) = after.find('/') {
                let mod_name = &after[..end];
                if !mod_name.is_empty() {
                    return Some(format!("mod-{}", mod_name));
                }
            }
        }
    }

    // Flat src/ layout: use the immediate parent directory name
    if let Some(parent) = source_path.parent() {
        if let Some(name) = parent.file_name().and_then(|n| n.to_str()) {
            if !name.is_empty() && name != "." && name != "src" {
                return Some(format!("mod-{}", name));
            }
        }
    }

    None
}

/// Auto-document a codebase: discover source files, skip unchanged,
/// emit structured contracts, and generate an index.
pub fn cmd_gen(
    path: &Path,
    out_dir: Option<&Path>,
    detect_out_dir: bool,
    auto: bool,
    merge: bool,
    propose: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.is_file() {
        let source = std::fs::read_to_string(path)?;
        let docs = aden_parse::parse_file(path, &source)?;

        let effective_out = if detect_out_dir {
            detect_contract_structure(path, path)
                .unwrap_or_else(|| Path::new("contracts").to_path_buf())
        } else {
            out_dir
                .unwrap_or_else(|| Path::new("contracts"))
                .to_path_buf()
        };

        if merge || propose {
            return cmd_gen_contract(path, &source, docs, Some(&effective_out), propose);
        }
        return emit_docs(docs, Some(&effective_out), path, format);
    }

    if !path.is_dir() {
        return Err("Path does not exist or is not a file/directory".into());
    }

    let root = find_project_root(path);
    let effective_out = out_dir.unwrap_or_else(|| Path::new("contracts"));

    // Auto-generate module contracts for each crate (deterministic)
    generate_module_contracts(&root, effective_out)?;

    if detect_out_dir && out_dir.is_none() {
        let mut auto_detected = false;
        if root.join("contracts").join("crates").exists() {
            let contracts_crates = root.join("contracts").join("crates");
            if contracts_crates.is_dir() {
                println!("INFO: Detected contracts/crates/ structure. Using --detect-out-dir.");
                println!("      Contracts will be placed in contracts/crates/<crate>/src/");
                auto_detected = true;
            }
        }
        if !auto_detected {
            println!("INFO: No existing contract structure detected. Using default ./contracts/");
        }
    }

    if merge || propose {
        return cmd_gen_merge(&root, effective_out, propose);
    }

    // Default to auto mode for directories (backward compatible with single files)
    let auto_by_default = path.is_dir();
    if auto || auto_by_default {
        // ── AUTO MODE: workspace-aware incremental generation ────────────────
        let sources = discover_source_files(&root)?;
        if sources.is_empty() {
            eprintln!(
                "No source files discovered in {}. Is this a supported project?",
                root.display()
            );
            return Ok(());
        }

        std::fs::create_dir_all(effective_out)?;

        let cache_path = root.join(".aden").join("gen-cache.json");
        let mut cache = load_gen_cache(&cache_path);
        let mut generated = Vec::new();
        let mut skipped = 0usize;

        for src_path in &sources {
            let rel = src_path.strip_prefix(&root).unwrap_or(src_path);
            let contract_rel = rel.with_extension("adoc");
            let contract_path = effective_out.join(&contract_rel);

            // Ensure parent dirs exist
            if let Some(parent) = contract_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Check mtime cache
            let src_mtime = src_path
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let entry = cache.entries.get(contract_path.to_string_lossy().as_ref());
            if let Some(e) = entry
                && e.source_mtime
                    == src_mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                && contract_path.exists()
            {
                skipped += 1;
                continue;
            }

            let source = match std::fs::read_to_string(src_path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(e) => {
                    eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                    continue;
                }
            };

            let docs = match aden_parse::parse_file(src_path, &source) {
                Ok(d) => d,
                Err(aden_core::Error::UnsupportedLanguage(_)) => continue,
                Err(e) => {
                    eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                    continue;
                }
            };

            for doc in &docs {
                let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
                let file_path = contract_path
                    .parent()
                    .unwrap_or(effective_out)
                    .join(&file_name);
                let mut doc_clone = doc.clone();
                sanitize_source_file(&mut doc_clone);
                
// Auto-link to parent module (infer from source path, not contract path)
                let parent_module = infer_parent_module_from_source(src_path);
                if let Some(ref pm) = parent_module {
                    doc_clone.blocks.push(aden_core::Block::Paragraph("== Relationships".to_string()));
                    doc_clone.blocks.push(aden_core::Block::DescriptionList(vec![
                        (format!("<<{},module>>", pm),
                         "This symbol is part of the parent module.".to_string())
                    ]));
                }
                
                std::fs::write(&file_path, aden_emit::emit_document(&doc_clone))?;
                generated.push(file_name.clone());
                println!("Emitted {}", file_path.display());

                // Update cache
                let mtime_secs = src_mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                cache.entries.insert(
                    file_path.to_string_lossy().to_string(),
                    GenCacheEntry {
                        source_mtime: mtime_secs,
                        source_path: src_path.to_string_lossy().to_string(),
                    },
                );
            }
        }

        save_gen_cache(&cache_path, &cache)?;

        // Generate index
        if !generated.is_empty() {
            let index_path = effective_out.join("INDEX.adoc");
            let mut index = String::new();
            index.push_str("= Contracts Index\n\n");
            index.push_str("Auto-generated by `aden gen --auto .`\n\n");
            index.push_str("|===\n|Symbol |File |Anchor\n");
            for name in &generated {
                index.push_str(&format!(
                    "|{} |{} |[[{}]]\n",
                    name,
                    name,
                    name.trim_end_matches(".adoc")
                ));
            }
            index.push_str("|===\n");
            std::fs::write(&index_path, index)?;
            println!("Generated index: {}", index_path.display());
        }

        println!(
            "\nGenerated {} contracts. Skipped {} unchanged files.",
            generated.len(),
            skipped
        );
    } else {
        // ── LEGACY MODE: flat parse_directory output ────────────────────────
        let docs = aden_parse::parse_directory(path)?;
        return emit_docs(docs, out_dir, path, format);
    }

    // Invalidate caches after generating contracts so next query rebuilds
    let cache_dir = path.join(".aden/cache");
    if cache_dir.exists() {
        let _ = std::fs::remove_dir_all(&cache_dir);
        let _ = std::fs::create_dir_all(&cache_dir);
    }

    // Report orphan symbols
    match aden_graph::graph::AdenGraph::build_from_directory(path) {
        Ok(graph) => {
            let orphans = graph.orphans();
            if !orphans.is_empty() {
                eprintln!("\nWARNING: {} orphan symbol(s) detected:", orphans.len());
                for orphan in orphans.iter().take(5) {
                    eprintln!("  - {}", orphan);
                }
                if orphans.len() > 5 {
                    eprintln!("  ... and {} more", orphans.len() - 5);
                }
                eprintln!("  Run 'aden heal . --gc' to auto-link or remove orphans");
            }
        }
        Err(e) => {
            eprintln!("Note: Could not check for orphans: {}", e);
        }
    }

    Ok(())
}

/// Single-file contract generation with three-way merge support.
pub fn cmd_gen_contract(
    _path: &Path,
    _source: &str,
    docs: Vec<aden_core::Document>,
    out_dir: Option<&Path>,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{
        ContractDocument, ContractState, MergeAction, ParseMode, parse_contract,
    };

    if docs.is_empty() {
        return Ok(());
    }

    let effective_out = out_dir.unwrap_or_else(|| Path::new("contracts"));
    std::fs::create_dir_all(effective_out)?;

    let contract_path = effective_out.join(format!("{}.adoc", sanitize_anchor(&docs[0].anchor)));

    // Ground: freshly generated contract from AST
    let ground_doc = ContractDocument::from_document(&docs[0]);

    // Base: last pure generated content (from .aden/contract-base/)
    let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
        if base_path.exists() {
            let existing = std::fs::read_to_string(&base_path)?;
            parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|_| ground_doc.clone())
        } else {
            ground_doc.clone()
        }
    } else {
        ground_doc.clone()
    };

    // Working: current contract file on disk (with possible human edits)
    let working_doc = if contract_path.exists() {
        let existing = std::fs::read_to_string(&contract_path)?;
        parse_contract(&existing, ParseMode::Permissive).unwrap_or_else(|e| {
            eprintln!(
                "WARN: Failed to parse existing contract {}: {}. Treating as fresh.",
                contract_path.display(),
                e
            );
            ground_doc.clone()
        })
    } else {
        ground_doc.clone()
    };

    let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
    let proposal = state.propose()?;

    if propose {
        println!("// Merge Proposal for {}", contract_path.display());
        println!(
            "//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
            proposal.preserved_count,
            proposal.updated_count,
            proposal.conflict_count,
            proposal.inserted_count,
            proposal.deleted_count
        );
        for action in &proposal.actions {
            match action {
                MergeAction::UpdateGenerated { index, .. } => {
                    println!("  UPDATE [generated] @ block {}", index);
                }
                MergeAction::PreserveHuman { index } => {
                    println!("  PRESERVE human/agent block @ {}", index);
                }
                MergeAction::Conflict { index, reason } => {
                    println!("  CONFLICT @ block {}: {}", index, reason);
                }
                MergeAction::InsertGenerated { after_index, .. } => {
                    println!("  INSERT [generated] after block {}", after_index);
                }
                MergeAction::DeleteGenerated { index, reason } => {
                    println!("  DELETE [generated] @ block {}: {}", index, reason);
                }
            }
        }
        return Ok(());
    }

    // Merge mode: apply and write
    let merged = state.apply(&proposal)?;
    let output = aden_emit::emit_contract_document(&merged);
    std::fs::write(&contract_path, output)?;
    println!("Merged contract: {}", contract_path.display());

    // Update base cache so next run has clean generated snapshot
    if let Some(base_path) = base_cache_path(&contract_path) {
        if let Some(parent) = base_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
    }

    Ok(())
}

/// Directory-mode contract generation with merge support.
pub fn cmd_gen_merge(
    root: &Path,
    effective_out: &Path,
    propose: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use aden_core::contract::{ContractDocument, ContractState, ParseMode, parse_contract};

    let sources = discover_source_files(root)?;
    if sources.is_empty() {
        eprintln!("No source files discovered in {}.", root.display());
        return Ok(());
    }

    std::fs::create_dir_all(effective_out)?;

    for src_path in &sources {
        let source = match std::fs::read_to_string(src_path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(e) => {
                eprintln!("WARN: Failed to read {}: {}", src_path.display(), e);
                continue;
            }
        };

        let docs = match aden_parse::parse_file(src_path, &source) {
            Ok(d) => d,
            Err(aden_core::Error::UnsupportedLanguage(_)) => continue,
            Err(e) => {
                eprintln!("WARN: Parse failed for {}: {}", src_path.display(), e);
                continue;
            }
        };

        for doc in &docs {
            let file_name = format!("{}.adoc", sanitize_anchor(&doc.anchor));
            let contract_path = effective_out.join(&file_name);

            let ground_doc = ContractDocument::from_document(doc);

            // Base: last pure generated content
            let base_doc = if let Some(base_path) = base_cache_path(&contract_path) {
                if base_path.exists() {
                    let existing = std::fs::read_to_string(&base_path)?;
                    parse_contract(&existing, ParseMode::Permissive)
                        .unwrap_or_else(|_| ground_doc.clone())
                } else {
                    ground_doc.clone()
                }
            } else {
                ground_doc.clone()
            };

            // Working: current contract file on disk
            let working_doc = if contract_path.exists() {
                let existing = std::fs::read_to_string(&contract_path)?;
                parse_contract(&existing, ParseMode::Permissive)
                    .unwrap_or_else(|_| ground_doc.clone())
            } else {
                ground_doc.clone()
            };

            let state = ContractState::new(ground_doc.clone(), base_doc, working_doc);
            let proposal = state.propose()?;

            if propose {
                println!("// Proposal: {}", contract_path.display());
                println!(
                    "//   Preserved: {} | Updated: {} | Conflicts: {} | Inserted: {} | Deleted: {}",
                    proposal.preserved_count,
                    proposal.updated_count,
                    proposal.conflict_count,
                    proposal.inserted_count,
                    proposal.deleted_count
                );
            } else {
                let merged = state.apply(&proposal)?;
                let output = aden_emit::emit_contract_document(&merged);
                std::fs::write(&contract_path, output)?;
                println!("Merged contract: {}", contract_path.display());

                // Update base cache
                if let Some(base_path) = base_cache_path(&contract_path) {
                    if let Some(parent) = base_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&base_path, aden_emit::emit_contract_document(&ground_doc))?;
                }
            }
        }
    }

    Ok(())
}
