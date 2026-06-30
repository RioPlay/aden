// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scope manifest: aden's deterministic, token-budgeted definition of a
//! task's context and file mandate.
//!
//! `aden scope` emits one manifest per task; `aden impact-diff --scope` consumes
//! it as the edit gate. This module owns the shared shape so producer and
//! consumer cannot drift. The contract is documented (consumer side) in the coxn
//! repo's `docs/contract.adoc`.

use aden_graph::{AdenEdge, AdenGraph, DocumentNode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

/// A deterministic, token-budgeted definition of what one task may touch.
///
/// Fields mirror the directional-harness research: seeds resolve to an expanded
/// anchor set, which projects to a disjoint file mandate, plus the `asm`
/// parameters for pre-assembling context under a budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeManifest {
    /// Task name; labels the scope and the gate verdict.
    pub name: String,
    /// Seed anchors the task is about (resolved by aden from task text).
    #[serde(default)]
    pub seeds: Vec<String>,
    /// Expanded anchor set: community ∪ transitive dependents ∪ depth-1 deps.
    #[serde(default)]
    pub anchors: Vec<String>,
    /// File mandate: the disjoint list of files the task may touch.
    #[serde(default)]
    pub files: Vec<String>,
    /// The `asm` parameters for pre-assembling this scope's context.
    #[serde(default)]
    pub context: ScopeContext,
    /// Risk score for the scope (`0` none, `<=5` low, `<=20` medium, else high).
    #[serde(default)]
    pub risk: u32,
}

/// The context-assembly parameters carried by a manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeContext {
    /// The anchor set to assemble.
    #[serde(default)]
    pub anchors: Vec<String>,
    /// The token ceiling for assembly.
    #[serde(default)]
    pub budget: u64,
}

impl ScopeManifest {
    /// Parse a manifest from JSON (as emitted by `aden scope`).
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// Serialize to pretty JSON for emission by `aden scope`.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Assemble a scope manifest from resolved seed anchors over the graph. Pure
/// over the graph (no I/O), so it is unit-testable with a fixture.
///
/// Per the directional-harness design, a seed's scope is its expanded anchor
/// set: the seed ∪ its transitive dependents (what breaks if it changes) ∪ its
/// depth-1 dependencies ∪ its community (related cluster). The anchor set
/// projects to a file mandate; risk is the dependent breadth.
pub(crate) fn assemble_scope(
    graph: &AdenGraph<DocumentNode, AdenEdge>,
    name: &str,
    seed_anchors: &[String],
    budget: u64,
    anchor_to_file: &HashMap<String, String>,
) -> ScopeManifest {
    let impact_types = crate::util::impact_edge_types();
    let communities = aden_graph::community::detect_communities(graph, 1.0);

    let mut anchors: BTreeSet<String> = BTreeSet::new();
    let mut dependents: BTreeSet<String> = BTreeSet::new();
    for seed in seed_anchors {
        anchors.insert(seed.clone());

        let deps = crate::commands::impact_diff::dependents_of(graph, seed, &impact_types);
        dependents.extend(deps.iter().cloned());
        anchors.extend(deps);

        if let Some(idx) = graph.get_index(seed) {
            for nb in graph
                .graph
                .neighbors_directed(idx, aden_graph::Direction::Outgoing)
            {
                anchors.insert(graph.graph[nb].doc.anchor.clone());
            }
        }

        if let Some(group) = communities.iter().find(|g| g.iter().any(|a| a == seed)) {
            anchors.extend(group.iter().cloned());
        }
    }

    // Project anchors to a repo-relative file mandate via the store's
    // `source_file` map — the same namespace `git diff` and the gate use. (The
    // anchor's own path component is project-prefixed and drops crate-root file
    // names, so it cannot be the mandate; the source_file map is authoritative.)
    let mut files: BTreeSet<String> = BTreeSet::new();
    for a in &anchors {
        if let Some(f) = anchor_to_file.get(a) {
            files.insert(f.clone());
        }
    }

    let anchors_vec: Vec<String> = anchors.into_iter().collect();
    ScopeManifest {
        name: name.to_string(),
        seeds: seed_anchors.to_vec(),
        anchors: anchors_vec.clone(),
        files: files.into_iter().collect(),
        context: ScopeContext {
            anchors: anchors_vec,
            budget,
        },
        risk: dependents.len() as u32,
    }
}

/// One entry in an agent partition.
///
/// Each sub-scope owns a disjoint file mandate and carries the role and
/// dependency ordering coxn needs to spawn and sequence per-scope pumps.
#[derive(Debug, Clone)]
pub struct SubScope {
    /// Stable identifier for this sub-scope (slug derived from task name + seed index).
    pub id: String,
    /// Role tag for model routing (scout / synth / orchestrate or user-defined).
    pub role: String,
    /// Repo-relative path to the manifest file written by `cmd_scope_agents`.
    pub manifest_path: String,
    /// IDs of sub-scopes this one depends on (must complete first).
    pub depends_on: Vec<String>,
    /// The manifest content for this sub-scope.
    pub manifest: ScopeManifest,
}

/// Partition a set of per-seed manifests into disjoint sub-scopes.
///
/// Heuristic: one sub-scope per seed (or per community if seeds share one).
/// Files are assigned to exactly one sub-scope -- the first seed (in
/// alphabetical order) whose expanded anchor set covers the file. This is
/// deterministic and keeps file ownership unambiguous for the gate.
///
/// Role assignment:
///   - A single seed sub-scope that has no dependents => "scout"
///   - Other seed sub-scopes => "synth"
///   - If more than one sub-scope is produced, a final merge entry tagged
///     "orchestrate" is appended that depends on all sub-scopes above it and
///     carries an empty file mandate (merge is a read-only concatenation step).
///
/// `depends_on` ordering: all scout/synth scopes are independent of each
/// other (they have disjoint files); only the orchestrate entry depends on
/// them, so parallel execution of the leaf scopes is safe.
pub(crate) fn partition_scope(
    seed_manifests: Vec<(String, ScopeManifest)>,
    task_name: &str,
    budget: u64,
) -> Vec<SubScope> {
    if seed_manifests.is_empty() {
        return Vec::new();
    }

    // Track which files have been claimed so far (alphabetical seed order gives
    // deterministic ownership when two seeds both cover the same file).
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut sub_scopes: Vec<SubScope> = Vec::new();

    // Sort seeds alphabetically for deterministic file assignment.
    let mut sorted = seed_manifests;
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    for (i, (_seed_anchor, mut manifest)) in sorted.into_iter().enumerate() {
        // Retain only files not yet owned by an earlier sub-scope.
        let disjoint_files: Vec<String> = manifest
            .files
            .iter()
            .filter(|f| !claimed.contains(*f))
            .cloned()
            .collect();
        for f in &disjoint_files {
            claimed.insert(f.clone());
        }
        manifest.files = disjoint_files.clone();

        // Slug: task_name with non-alphanumeric chars replaced, plus index.
        let slug: String = task_name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let id = format!("{slug}-{i}");

        // Scout if first, synth otherwise.
        let role = if i == 0 {
            "scout".to_string()
        } else {
            "synth".to_string()
        };

        sub_scopes.push(SubScope {
            id,
            role,
            manifest_path: String::new(), // filled in by cmd_scope_agents
            depends_on: Vec::new(),       // leaf scopes are independent of each other
            manifest,
        });
    }

    // If more than one sub-scope, append a merge/orchestrate entry that depends
    // on all leaf scopes and carries no file mandate of its own.
    if sub_scopes.len() > 1 {
        let dep_ids: Vec<String> = sub_scopes.iter().map(|s| s.id.clone()).collect();
        let slug: String = task_name
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let merge_id = format!("{slug}-merge");
        let merge_manifest = ScopeManifest {
            name: format!("{task_name} (merge)"),
            seeds: Vec::new(),
            anchors: Vec::new(),
            files: Vec::new(),
            context: ScopeContext {
                anchors: Vec::new(),
                budget,
            },
            risk: 0,
        };
        sub_scopes.push(SubScope {
            id: merge_id,
            role: "orchestrate".to_string(),
            manifest_path: String::new(),
            depends_on: dep_ids,
            manifest: merge_manifest,
        });
    }

    sub_scopes
}

/// `aden scope --agents`: partition a task into disjoint sub-scopes, write one
/// manifest file per sub-scope under `.aden/agents/`, and emit the partition
/// index to stdout (one tab-separated record per sub-scope).
///
/// Index format (per the coxn routing contract, section 3):
///
///   <id>\t<role>\t<manifest-path>\t<comma-separated depends_on ids>
///
/// The `--agents` path does not change `--json` behavior when `--agents` is
/// absent; existing callers are unaffected.
pub fn cmd_scope_agents(
    path: &Path,
    name: &str,
    seeds: &[String],
    budget: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate::util::find_project_root(path);
    crate::commands::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;

    let all_anchors: Vec<String> = graph
        .graph
        .node_indices()
        .map(|i| graph.graph[i].doc.anchor.clone())
        .collect();

    // Resolve each seed symbol to a canonical anchor.
    let mut seed_anchors: Vec<String> = Vec::new();
    for s in seeds {
        let resolved = if graph.get_index(s).is_some() {
            Some(s.clone())
        } else {
            crate::commands::locate::pick_symbol_anchor(s, &all_anchors)
        };
        match resolved {
            Some(a) => seed_anchors.push(a),
            None => {
                return Err(
                    format!("seed '{s}' did not resolve to any symbol in the store").into(),
                );
            }
        }
    }
    seed_anchors.sort();
    seed_anchors.dedup();

    // anchor -> repo-relative source file map (same namespace as the gate).
    let mut anchor_to_file: HashMap<String, String> = HashMap::new();
    for (file, spans) in crate::commands::grep::load_symbol_spans(&root) {
        for sp in spans {
            anchor_to_file.insert(sp.anchor, file.clone());
        }
    }

    // Build one manifest per resolved seed.
    let seed_manifests: Vec<(String, ScopeManifest)> = seed_anchors
        .iter()
        .map(|seed| {
            let m = assemble_scope(
                &graph,
                name,
                std::slice::from_ref(seed),
                budget,
                &anchor_to_file,
            );
            (seed.clone(), m)
        })
        .collect();

    let mut sub_scopes = partition_scope(seed_manifests, name, budget);

    // Write manifests and populate manifest_path on each sub-scope.
    let agents_dir = root.join(".aden").join("agents");
    std::fs::create_dir_all(&agents_dir)?;

    for ss in &mut sub_scopes {
        let filename = format!("{}.json", ss.id);
        let abs_path = agents_dir.join(&filename);
        // Manifest path is repo-relative (same convention as `aden scope --json`).
        let rel_path = format!(".aden/agents/{filename}");
        ss.manifest.name = ss.id.clone();
        std::fs::write(&abs_path, ss.manifest.to_json()?)?;
        ss.manifest_path = rel_path;
    }

    // Emit the partition index to stdout.
    for ss in &sub_scopes {
        println!(
            "{}\t{}\t{}\t{}",
            ss.id,
            ss.role,
            ss.manifest_path,
            ss.depends_on.join(",")
        );
    }

    Ok(())
}

/// `aden scope`: emit the deterministic, token-budgeted scope manifest for a
/// task. Read-only -- it reads the existing graph and writes nothing.
pub fn cmd_scope(
    path: &Path,
    name: &str,
    seeds: &[String],
    budget: u64,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let root = crate::util::find_project_root(path);
    crate::commands::ensure_fresh(&root);
    let graph = aden_graph::cache::build_from_directory_cached(&root)?;

    let all_anchors: Vec<String> = graph
        .graph
        .node_indices()
        .map(|i| graph.graph[i].doc.anchor.clone())
        .collect();

    // Resolve each seed: accept a full anchor as-is, else match a bare symbol
    // name through the shared locate resolver.
    let mut seed_anchors: Vec<String> = Vec::new();
    for s in seeds {
        let resolved = if graph.get_index(s).is_some() {
            Some(s.clone())
        } else {
            crate::commands::locate::pick_symbol_anchor(s, &all_anchors)
        };
        match resolved {
            Some(a) => seed_anchors.push(a),
            None => {
                return Err(
                    format!("seed '{s}' did not resolve to any symbol in the store").into(),
                );
            }
        }
    }
    seed_anchors.sort();
    seed_anchors.dedup();

    // anchor → repo-relative source file, inverted from the store's symbol spans
    // (the namespace `git diff` and `impact-diff --scope` compare against).
    let mut anchor_to_file: HashMap<String, String> = HashMap::new();
    for (file, spans) in crate::commands::grep::load_symbol_spans(&root) {
        for sp in spans {
            anchor_to_file.insert(sp.anchor, file.clone());
        }
    }

    let manifest = assemble_scope(&graph, name, &seed_anchors, budget, &anchor_to_file);

    if json {
        println!("{}", manifest.to_json()?);
    } else {
        println!(
            "scope '{}': {} seed(s) → {} anchor(s), {} file(s), risk {}",
            manifest.name,
            manifest.seeds.len(),
            manifest.anchors.len(),
            manifest.files.len(),
            manifest.risk
        );
        for f in &manifest.files {
            println!("  {f}");
        }
        println!("(run with --json to emit the manifest for `impact-diff --scope`)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_manifest() {
        let json = r#"{
            "name": "lint-json",
            "seeds": ["cmd_lint"],
            "anchors": ["cmd_lint", "fmt_report"],
            "files": ["src/lint.rs"],
            "context": { "anchors": ["cmd_lint"], "budget": 8192 },
            "risk": 3
        }"#;
        let m = ScopeManifest::from_json(json).unwrap();
        assert_eq!(m.name, "lint-json");
        assert_eq!(m.files, vec!["src/lint.rs".to_string()]);
        assert_eq!(m.context.budget, 8192);
        assert_eq!(m.risk, 3);
    }

    #[test]
    fn tolerates_minimal_manifest() {
        // Only `name` is required; everything else defaults empty.
        let m = ScopeManifest::from_json(r#"{"name":"x"}"#).unwrap();
        assert_eq!(m.name, "x");
        assert!(m.files.is_empty());
        assert_eq!(m.risk, 0);
    }

    fn fixture_node(anchor: &str) -> DocumentNode {
        DocumentNode {
            doc: aden_core::Document {
                anchor: anchor.to_string(),
                node_type: aden_core::NodeType::Function,
                attributes: std::collections::HashMap::new(),
                blocks: Vec::new(),
                source_span: None,
                metadata: None,
                confidence: 0.9,
            },
            parsed: None,
            source_path: std::path::PathBuf::from("x.adoc"),
        }
    }

    fn fixture_graph(
        anchors: &[&str],
        edges: &[(&str, &str)],
    ) -> AdenGraph<DocumentNode, AdenEdge> {
        let mut g = AdenGraph::new();
        for a in anchors {
            g.add_node(fixture_node(a));
        }
        for (src, tgt) in edges {
            g.add_edge_by_anchor(
                src,
                tgt,
                AdenEdge {
                    edge_type: aden_core::EdgeType::Calls,
                },
            )
            .unwrap();
        }
        g
    }

    #[test]
    fn assemble_expands_seed_into_dependents_deps_and_files() {
        // caller --Calls--> seed --Calls--> callee
        let caller = "aden://module/p/a.rs#caller";
        let seed = "aden://module/p/b.rs#seed";
        let callee = "aden://module/p/c.rs#callee";
        let g = fixture_graph(&[caller, seed, callee], &[(caller, seed), (seed, callee)]);

        // Repo-relative source files, as the store's source_file map gives.
        let a2f = HashMap::from([
            (caller.to_string(), "src/a.rs".to_string()),
            (seed.to_string(), "src/b.rs".to_string()),
            (callee.to_string(), "src/c.rs".to_string()),
        ]);
        let m = assemble_scope(&g, "task", &[seed.to_string()], 4096, &a2f);

        assert_eq!(m.seeds, vec![seed.to_string()]);
        // seed, its dependent (caller), and its depth-1 dependency (callee).
        assert!(m.anchors.contains(&caller.to_string()), "dependent missing");
        assert!(m.anchors.contains(&seed.to_string()), "seed missing");
        assert!(
            m.anchors.contains(&callee.to_string()),
            "dependency missing"
        );
        // Files are the repo-relative mandate (matches git diff + the gate).
        assert!(m.files.contains(&"src/a.rs".to_string()));
        assert!(m.files.contains(&"src/b.rs".to_string()));
        // Risk is the dependent breadth: only `caller` depends on `seed`.
        assert_eq!(m.risk, 1);
        assert_eq!(m.context.budget, 4096);
        assert!(!m.context.anchors.is_empty());
    }

    // -- partition_scope tests --------------------------------------------------

    fn make_manifest(name: &str, files: &[&str]) -> ScopeManifest {
        ScopeManifest {
            name: name.to_string(),
            seeds: vec![name.to_string()],
            anchors: vec![name.to_string()],
            files: files.iter().map(|f| f.to_string()).collect(),
            context: ScopeContext {
                anchors: vec![name.to_string()],
                budget: 4096,
            },
            risk: 0,
        }
    }

    #[test]
    fn partition_single_seed_emits_one_scope_no_merge() {
        // A single seed produces exactly one sub-scope (scout); no merge entry.
        let seed_manifests = vec![("seed-a".to_string(), make_manifest("task", &["src/a.rs"]))];
        let scopes = partition_scope(seed_manifests, "task", 4096);
        assert_eq!(scopes.len(), 1, "single seed => one scope, no merge");
        assert_eq!(scopes[0].role, "scout");
        assert!(scopes[0].depends_on.is_empty());
    }

    #[test]
    fn partition_two_seeds_emits_leaf_scopes_plus_merge() {
        // Two seeds => two leaf scopes + one orchestrate merge entry.
        let seed_manifests = vec![
            (
                "seed-a".to_string(),
                make_manifest("task", &["src/a.rs", "src/shared.rs"]),
            ),
            (
                "seed-b".to_string(),
                make_manifest("task", &["src/b.rs", "src/shared.rs"]),
            ),
        ];
        let scopes = partition_scope(seed_manifests, "task", 4096);
        assert_eq!(scopes.len(), 3, "two seeds + merge entry");

        let merge = scopes.last().unwrap();
        assert_eq!(merge.role, "orchestrate");
        assert_eq!(merge.depends_on.len(), 2);
    }

    #[test]
    fn partition_mandates_are_disjoint() {
        // No file may appear in two sub-scopes' mandates.
        let seed_manifests = vec![
            (
                "seed-a".to_string(),
                make_manifest("task", &["src/a.rs", "src/shared.rs"]),
            ),
            (
                "seed-b".to_string(),
                make_manifest("task", &["src/b.rs", "src/shared.rs"]),
            ),
            (
                "seed-c".to_string(),
                make_manifest("task", &["src/c.rs", "src/shared.rs"]),
            ),
        ];
        let scopes = partition_scope(seed_manifests, "task", 4096);

        // Collect all files from leaf scopes (exclude the merge scope).
        let leaf_scopes: Vec<&SubScope> =
            scopes.iter().filter(|s| s.role != "orchestrate").collect();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for ss in &leaf_scopes {
            for f in &ss.manifest.files {
                assert!(
                    seen.insert(f.clone()),
                    "file '{f}' appears in more than one sub-scope mandate"
                );
            }
        }
        // shared.rs must appear in exactly one sub-scope.
        assert!(
            seen.contains("src/shared.rs"),
            "shared file must be assigned to one scope"
        );
    }

    #[test]
    fn partition_roles_are_scout_then_synth() {
        // First sub-scope (alphabetical by seed) is scout; remainder are synth.
        let seed_manifests = vec![
            ("seed-b".to_string(), make_manifest("task", &["src/b.rs"])),
            ("seed-a".to_string(), make_manifest("task", &["src/a.rs"])),
        ];
        let scopes = partition_scope(seed_manifests, "task", 4096);
        // Leaf scopes (not orchestrate).
        let leafs: Vec<&SubScope> = scopes.iter().filter(|s| s.role != "orchestrate").collect();
        assert_eq!(leafs[0].role, "scout");
        assert_eq!(leafs[1].role, "synth");
    }

    #[test]
    fn non_agents_path_unchanged() {
        // Verify that assembling a single manifest (the non-agents path) is
        // stable -- same fields as before this patch.
        let caller = "aden://module/p/a.rs#caller";
        let seed = "aden://module/p/b.rs#seed";
        let g = fixture_graph(&[caller, seed], &[(caller, seed)]);
        let a2f = HashMap::from([
            (caller.to_string(), "src/a.rs".to_string()),
            (seed.to_string(), "src/b.rs".to_string()),
        ]);
        let m = assemble_scope(&g, "non-agents-task", &[seed.to_string()], 8192, &a2f);
        // Basic shape unchanged.
        assert_eq!(m.name, "non-agents-task");
        assert_eq!(m.seeds, vec![seed.to_string()]);
        assert_eq!(m.context.budget, 8192);
        assert!(m.files.contains(&"src/b.rs".to_string()));
    }
}
