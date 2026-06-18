// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Shared NL→symbol retrieval eval probe set: (query, accept gold symbols, oracle expansion).
// Queries are SYNONYM-MISMATCH (never contain a gold's sub-tokens) and every gold is a real,
// verified aden symbol. Lives under tests/ on purpose — the retrieval harnesses exclude
// `/tests/` cards via the leak filter, so this file can't pollute the corpus it evaluates.
#![allow(dead_code)]

pub type Probe = (&'static str, &'static [&'static str], &'static str);

pub const PROBES: &[Probe] = &[
    (
        "store a batch of relationships between nodes in one operation",
        &["put_edges_bulk"],
        "append bulk typed edges deduplicate",
    ),
    (
        "group the graph into clusters of tightly connected nodes",
        &["detect_communities"],
        "community detection louvain modularity",
    ),
    (
        "blend two ranked result lists into a single ordering",
        &["rrf_fuse"],
        "reciprocal rank fusion combine rankings",
    ),
    (
        "how aligned are two embedding vectors",
        &["cosine_similarity"],
        "cosine similarity vector",
    ),
    (
        "fewest single character edits to turn one word into another",
        &["levenshtein_distance"],
        "levenshtein edit distance",
    ),
    (
        "figure out which definition a function call points to",
        &["resolve_callee"],
        "resolve callee definition anchor",
    ),
    (
        "decide what category of question the user is asking",
        &["classify_intent"],
        "classify intent query category",
    ),
    (
        "detect a leaked password or api key inside text",
        &["content_has_high_confidence_secret"],
        "secret credential api key detection",
    ),
    (
        "collect the nodes surrounding a starting symbol up to some depth",
        &["build_neighborhood"],
        "neighborhood traversal depth graph",
    ),
    (
        "find everything that points at a given node",
        &["get_incoming_edges"],
        "incoming edges backlinks callers references",
    ),
    (
        "how many tokens were avoided versus reading whole files",
        &["SavingsEstimate"],
        "savings estimate tokens baseline bytes",
    ),
    (
        "anchors in the graph that nothing else references",
        &["scan_orphans"],
        "scan orphan anchors unreferenced dangling",
    ),
    (
        "remove formatting noise from a documentation string so an LLM receives only semantic content with no structural overhead",
        &["strip_asciidoc_markup"],
        "markup tables delimiters anchor llm",
    ),
    (
        "gather a subgraph around a starting node into a text prompt and return the list of included node identifiers in visit order",
        &["assemble_with_anchors"],
        "bfs neighborhood context budget traversal",
    ),
    (
        "find relevant symbols by combining keyword ranking with vector similarity and merging the two result lists",
        &["hybrid_query"],
        "bm25 dense rrf retrieval fuse ranking",
    ),
    (
        "run a neural encoder over every indexed contract and persist the resulting vectors for future similarity lookups",
        &["embed_documents"],
        "bge onnx corpus incremental provider vectors",
    ),
    (
        "produce the canonical form of a contract used for fingerprinting and encoding by dropping the provenance attributes that change on every run",
        &["stable_embed_text"],
        "last-verified span source_hash projection",
    ),
    (
        "expand a single search term into all its equivalent canonical representations such as numbers months ordinals and booleans",
        &["SemanticNormalizer"],
        "canonical bm25 temporal ordinal synonym",
    ),
    (
        "break a camelCase or snake-case identifier into its component lowercase words",
        &["split_subtokens"],
        "separator identifier components word humps",
    ),
    (
        "check whether a query word lines up with an identifier's word edges rather than appearing only as a raw interior substring",
        &["token_boundary_match"],
        "edge subword camelcase prefix",
    ),
    (
        "perform a three-way merge of a freshly-parsed symbol against the stored base and the human-intent overlay to produce a conflict-free result",
        &["reconcile_contract"],
        "ground base working overlay three-way merge",
    ),
    (
        "determine whether a pending three-way reconciliation has no outstanding conflicts and is safe to apply automatically",
        &["is_clean"],
        "conflict-free auto-apply actions",
    ),
    (
        "read a region-tagged AsciiDoc text into a structured in-memory representation of generated and human blocks",
        &["parse_contract"],
        "region block asciidoc strict permissive",
    ),
    (
        "serialize the in-memory form of generated and human regions back to region-tagged AsciiDoc for storage",
        &["emit_contract_document"],
        "region block asciidoc serializer canonical",
    ),
    (
        "fingerprint a file's contents with line endings normalised so identical content yields the same value on Windows and Linux",
        &["hash_source"],
        "crlf lf normalization change-detection drift",
    ),
    (
        "load a knowledge-graph node from raw bytes and reconstruct its line-range metadata from stored attributes when the struct field is absent",
        &["deserialize_document"],
        "rehydrate source_span postcard attributes",
    ),
    (
        "drop the redundant callee-listing block from a knowledge node before persisting it on disk to keep its size down",
        &["slim_doc_for_store"],
        "callee listing block size redundant",
    ),
    (
        "remove the absolute host path prefix from a document's path attribute so no username or home directory leaks into stored or model-visible context",
        &["sanitize_source_file"],
        "absolute path prefix strip security context",
    ),
    (
        "show which symbols transitively depend on code touched by the current git working-tree changes together with covering tests",
        &["cmd_impact_diff"],
        "blast radius dependents git transitive tests",
    ),
    (
        "remove from the graph store all nodes whose originating file no longer exists on disk",
        &["cmd_heal_gc"],
        "orphaned stale node prune sweep deleted",
    ),
    (
        "create a structured fix suggestion for a detected contract drift event optionally using the three-way merge engine when the anchor is in the store",
        &["generate_proposal"],
        "drift event fix suggestion anchor",
    ),
    (
        "overwrite the content-fingerprint line in a contract file with the current value to resolve a drift warning",
        &["apply_stale_hash"],
        "source_hash fingerprint line overwrite drift",
    ),
    (
        "write a new generated region block into a source file that lacks its corresponding documentation node",
        &["apply_missing_contract"],
        "absent block region documentation node",
    ),
    (
        "given a categorized query goal return the set of graph relationship kinds most relevant to traverse",
        &["edge_types_for_intent"],
        "queryintent traversal relationship category",
    ),
    (
        "given a categorized query goal return the maximum number of hops to traverse during context assembly",
        &["depth_for_intent"],
        "queryintent traversal hops budget maximum",
    ),
    (
        "given a categorized query goal return which AST node kinds to include when building the context window",
        &["block_filter_for_intent"],
        "queryintent blockkind admonition paragraph",
    ),
    (
        "run a cheap file-modification sweep and if any source changed silently re-index just those files before serving a read command",
        &["ensure_fresh"],
        "mtime sweep incremental reindex stale",
    ),
    (
        "given a free-text description of what a user wants to do print the aden subcommand that best matches",
        &["cmd_suggest"],
        "subcommand recommendation free-text",
    ),
    (
        "access an already-built key-value store at a path returning an error rather than creating one when the directory is absent",
        &["open_existing"],
        "lsm fjall read-only absent notfound",
    ),
    (
        "a struct wrapping an ONNX runtime session and tokenizer that turns text strings into dense float vectors",
        &["TractEmbedder"],
        "onnx runtime tokenizer inference dense float32",
    ),
];
