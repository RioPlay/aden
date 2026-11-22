// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use aden_core::source::StoredSourceReader;
use aden_core::{Block, Document, NodeType, SourceSpan};
use aden_graph::cache::GraphReadSession;
use aden_index::{Index, SearchResult, tokenize};

use super::{AnchorPattern, is_test_result};

/// Closely related terms for recognizing and probing a concrete operation.
/// Only the selected group participates, and probes use indexed vocabulary.
const ACTIONS: &[&[&str]] = &[
    &["publish", "rename", "replace"],
    &["resume", "continue", "continuation"],
    &[
        "choose", "pick", "select", "resolve", "decide", "prefer", "fallback",
    ],
    &["fuse", "fusion", "combine", "merge"],
    &["obtain", "derive", "locate", "get"],
    &["write", "save", "persist", "serialize"],
    &["validate", "verify", "check"],
    &["calculate", "compute", "measure"],
    &["parse", "decode", "deserialize"],
];

const FRAME_WORDS: &[&str] = &[
    "where",
    "which",
    "how",
    "what",
    "why",
    "when",
    "does",
    "did",
    "can",
    "should",
    "the",
    "code",
    "function",
    "method",
    "implementation",
    "implement",
    "part",
    "work",
    "actual",
    "handle",
    "between",
    "whether",
];

const MAX_ORIGINAL: usize = 36;
const MAX_PROBE: usize = 16;
const MAX_DOCS: usize = 4;
const MAX_HINTS: usize = 8;

pub(super) struct Operation {
    // Keep the user's action distinct from weaker related vocabulary.
    action: String,
    family: Vec<String>,
    objects: Vec<String>,
    pub(super) passive: bool,
}

pub(super) struct OperationRoute {
    pub anchor: String,
    pub support: Vec<String>,
    pub confidence: &'static str,
    pub reason: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Fit {
    strength: usize,
    precise: bool,
    name_fit: bool,
}

fn intersects(terms: &[String], family: &[String]) -> bool {
    terms.iter().any(|term| family.contains(term))
}

fn description(doc: &Document) -> &str {
    doc.blocks
        .iter()
        .find_map(|block| match block {
            Block::Paragraph(text) => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default()
}

fn predicate(words: &[&str]) -> Option<usize> {
    let function = ["code", "function", "method", "implementation", "part"];
    let articles = ["the", "a", "an", "this", "that", "its"];
    let prepositions = ["between", "whether", "which"];
    let frames = ["is", "are", "was", "were", "responsible", "named", "work"];

    if words.get(1).is_some_and(|word| function.contains(word))
        && words.get(2).is_some_and(|word| !frames.contains(word))
    {
        return Some(2);
    }
    let aux = words
        .iter()
        .position(|word| matches!(*word, "does" | "do" | "did"))?;
    let subject = aux + 1 + usize::from(words.get(aux + 1).is_some_and(|w| articles.contains(w)));
    let start = subject + 1;
    let actor_nouns = [
        "app",
        "application",
        "client",
        "component",
        "engine",
        "library",
        "server",
        "service",
        "system",
        "worker",
    ];
    // An object determiner or governed preposition can disambiguate a
    // multi-word subject. With simple subject–verb phrasing, use position.
    (start..words.len())
        .find(|&i| articles.contains(&words[i]) || prepositions.contains(&words[i]))
        .and_then(|boundary| {
            boundary.checked_sub(1).filter(|&i| {
                i >= start
                    && !matches!(
                        words[i],
                        "of" | "in" | "into" | "on" | "as" | "from" | "for"
                    )
            })
        })
        .or_else(|| {
            let possible_verb =
                start + usize::from(words.get(start).is_some_and(|w| actor_nouns.contains(w)));
            words.get(possible_verb).map(|_| possible_verb)
        })
        .filter(|&i| !frames.contains(&words[i]))
}

impl Operation {
    pub(super) fn parse(question: &str) -> Option<Self> {
        let lower = question.to_lowercase();
        if ["how do i ", "how do we "]
            .iter()
            .any(|prefix| lower.trim_start().starts_with(prefix))
        {
            return None;
        }
        if ![
            "where ",
            "which ",
            "how does ",
            "how do ",
            "how is ",
            "how are ",
            "what code ",
            "what function ",
            "what method ",
            "what implements ",
            "what part ",
        ]
        .iter()
        .any(|prefix| lower.trim_start().starts_with(prefix))
        {
            return None;
        }
        let words: Vec<_> = lower
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .filter(|word| !word.is_empty())
            .collect();
        if words
            .iter()
            .any(|word| matches!(*word, "defined" | "definition" | "located"))
        {
            return None;
        }
        let families: Vec<_> = ACTIONS
            .iter()
            .map(|family| {
                family
                    .iter()
                    .map(|word| {
                        tokenize(word)
                            .into_iter()
                            .next()
                            .unwrap_or_else(|| (*word).into())
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let (action, objects_text, passive) = if let Some(position) = predicate(&words) {
            let word = words[position];
            if word == "i" || word == "we" || FRAME_WORDS.contains(&word) {
                return None;
            }
            (
                tokenize(word)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| word.into()),
                words[position + 1..].join(" "),
                false,
            )
        } else {
            // Passive and nominal requests ("where is rank fusion
            // implemented?") have no verb in the active predicate position.
            let action = words
                .iter()
                .filter(|word| word.chars().all(char::is_alphabetic))
                .flat_map(|word| tokenize(word))
                .find(|term| families.iter().any(|family| family.contains(term)))?;
            let passive = ["where is ", "where are ", "how is ", "how are "]
                .iter()
                .any(|prefix| lower.trim_start().starts_with(prefix))
                && !words.contains(&"implemented");
            (action, question.to_string(), passive)
        };
        let family = families
            .into_iter()
            .find(|family| family.contains(&action))
            .unwrap_or_else(|| vec![action.clone()]);
        let frame = tokenize(&FRAME_WORDS.join(" "));
        let mut objects = Vec::new();
        for term in tokenize(&objects_text) {
            if !family.contains(&term) && !frame.contains(&term) && !objects.contains(&term) {
                objects.push(term);
            }
        }
        (!objects.is_empty()).then_some(Self {
            action,
            family,
            objects,
            passive,
        })
    }

    fn fit(
        &self,
        name: &str,
        summary: &str,
        module: &str,
        documented_objects: &[String],
    ) -> Option<Fit> {
        let name = tokenize(name);
        let summary = tokenize(summary);
        let module = tokenize(module);
        let accessor = self.family.iter().any(|term| term == "get")
            && self
                .objects
                .iter()
                .any(|term| matches!(term.as_str(), "filenam" | "file" | "path"))
            && name
                .iter()
                .any(|term| matches!(term.as_str(), "filenam" | "file" | "path"));
        let name_action = accessor || intersects(&name, &self.family);
        // A summary can describe an adjacent operation in the passive voice
        // ("the record written on shutdown"). Only its leading predicate can
        // establish that this function performs the action.
        let summary_action = summary
            .iter()
            .find(|term| !matches!(term.as_str(), "function" | "method" | "atom"))
            .is_some_and(|term| self.family.contains(term));
        let names_object = intersects(&name, &self.objects);
        let describes_object = intersects(&summary, &self.objects);
        let module_object = intersects(&module, &self.objects);
        let names_documented_object = intersects(&name, documented_objects);
        if !(name_action
            && (names_object || describes_object || module_object || names_documented_object)
            || summary_action && describes_object)
        {
            return None;
        }
        let exact_action = name.contains(&self.action)
            || ((summary_action || name_action) && summary.contains(&self.action));
        let object_strength: usize = self
            .objects
            .iter()
            .map(|term| {
                if name.contains(term) {
                    4
                } else if summary.contains(term) {
                    3
                } else {
                    usize::from(module.contains(term))
                }
            })
            .sum();
        Some(Fit {
            strength: object_strength
                + usize::from(name_action) * 5
                + usize::from(summary_action) * 3
                + usize::from(exact_action) * 3
                + usize::from(names_documented_object) * 4,
            precise: exact_action
                && (name_action && names_object || describes_object)
                && self.objects.iter().take(3).all(|term| {
                    name.contains(term) || summary.contains(term) || module.contains(term)
                }),
            name_fit: name_action && names_object,
        })
    }

    pub(super) fn route(
        &self,
        index: &Index,
        path: &Path,
        session: &GraphReadSession<'_>,
        original: &[SearchResult],
    ) -> Option<OperationRoute> {
        let docs = original
            .iter()
            .filter(|result| AnchorPattern::is_prose_doc(&result.anchor))
            .take(MAX_DOCS);
        let mut hints = Vec::new();
        let mut command = None;
        let mut mention_hints = Vec::new();
        let mut documented_objects = Vec::new();
        for result in docs {
            let Some(doc) = session.document(&result.anchor).ok().flatten() else {
                continue;
            };
            if let Some(mentions) = doc.attributes.get("doc_mentions") {
                for mention in mentions.split(',').take(24) {
                    mention_hints.extend(
                        contract_identifiers(mention)
                            .into_iter()
                            .filter(|hint| index.knows_term(hint))
                            .map(|hint| (hint, result.anchor.clone())),
                    );
                    let terms = tokenize(mention);
                    if intersects(&terms, &self.family) {
                        for term in terms.into_iter().skip(1) {
                            if documented_objects.len() < 2
                                && !self.family.contains(&term)
                                && !self.objects.contains(&term)
                                && !documented_objects.contains(&term)
                                && index.knows_term(&term)
                            {
                                documented_objects.push(term);
                            }
                        }
                    }
                }
            }
            // A documentation mention is only a lead. Keep a small set of
            // concrete identifiers that can be checked in an implementation.
            let mut remaining = 8192;
            for text in doc.blocks.iter().take(32).filter_map(|block| match block {
                Block::Paragraph(text) => Some(text.as_str()),
                Block::Listing { code, .. } => Some(code.as_str()),
                _ => None,
            }) {
                if remaining == 0 {
                    break;
                }
                let mut end = text.len().min(remaining);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                remaining -= end;
                let text = &text[..end];
                // Manuals often introduce a command before describing its flags.
                // The name supplies a candidate only; its source must also match.
                if command.is_none() {
                    command = text.split('`').skip(1).step_by(2).find_map(|example| {
                        let mut words = example.split_whitespace();
                        let program = words.next()?;
                        let verb = words.next()?;
                        let is_name = |s: &str| {
                            s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                        };
                        (is_name(program) && is_name(verb)).then(|| verb.replace('-', "_"))
                    });
                }
                let lines: Vec<_> = text.lines().take(32).collect();
                for (line, text) in lines.iter().enumerate() {
                    let terms = tokenize(text);
                    if !intersects(&terms, &self.family) {
                        continue;
                    }
                    // Command arguments may appear on the next line after
                    // the paragraph introduces the behavior.
                    let nearby = lines[line.saturating_sub(1)..=(line + 1).min(lines.len() - 1)]
                        .iter()
                        .flat_map(|text| contract_identifiers(text));
                    for hint in nearby {
                        if index.knows_term(&hint)
                            && !hints
                                .iter()
                                .any(|(name, anchor)| name == &hint && anchor == &result.anchor)
                        {
                            hints.push((hint, result.anchor.clone()));
                        }
                    }
                }
            }
        }
        for hint in mention_hints {
            if !hints.contains(&hint) {
                hints.push(hint);
            }
        }
        hints.truncate(MAX_HINTS);

        let objects = self
            .objects
            .iter()
            .take(8)
            .chain(&documented_objects)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        // Try one focused lexical probe when the original ranking has not
        // already surfaced multiple well-supported candidates. The second
        // probe, when available, follows the documentation's exact names.
        let enough_original = original
            .iter()
            .take(8)
            .filter(|result| {
                !is_test_result(result) && !AnchorPattern::is_prose_doc(&result.anchor)
            })
            .filter(|result| {
                let Some((module, name)) = result.anchor.rsplit_once('#') else {
                    return false;
                };
                session
                    .document(&result.anchor)
                    .ok()
                    .flatten()
                    .is_some_and(|doc| {
                        doc.node_type == NodeType::Function
                            && self
                                .fit(name, description(&doc), module, &[])
                                .is_some_and(|fit| fit.precise)
                    })
            })
            .take(if self.family.len() == 1 { 1 } else { 2 })
            .count()
            >= if self.family.len() == 1 { 1 } else { 2 };
        let mut probes = Vec::new();
        let actions = self
            .family
            .iter()
            .filter(|term| index.knows_term(term))
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        if !enough_original && !actions.is_empty() {
            probes.push(format!("{actions} {objects}"));
        }
        if !enough_original && !hints.is_empty() {
            probes.push(
                hints
                    .iter()
                    .take(4)
                    .map(|(term, _)| term.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        let mut candidates: Vec<SearchResult> =
            original.iter().take(MAX_ORIGINAL).cloned().collect();
        for probe in probes {
            candidates.extend(
                index
                    .query(&probe)
                    .into_iter()
                    .filter(|result| !AnchorPattern::is_prose_doc(&result.anchor))
                    .take(MAX_PROBE),
            );
        }

        let mut seen = HashSet::new();
        let mut ranked = Vec::new();
        let mut related = Vec::new();
        for (rank, result) in candidates.into_iter().enumerate() {
            let Some((module, name)) = result.anchor.rsplit_once('#') else {
                continue;
            };
            if AnchorPattern::is_prose_doc(&result.anchor)
                || is_test_result(&result)
                || !seen.insert(result.anchor.clone())
            {
                continue;
            }
            let Some(doc) = session.document(&result.anchor).ok().flatten() else {
                continue;
            };
            if doc.node_type != NodeType::Function {
                continue;
            }
            if let Some(fit) = self.fit(name, description(&doc), module, &documented_objects) {
                ranked.push((fit, rank, result.anchor));
            } else if related.len() < 8
                && (intersects(&tokenize(name), &self.objects)
                    || intersects(&tokenize(module), &self.objects)
                    || intersects(&tokenize(description(&doc)), &self.objects))
            {
                related.push((result.anchor, doc));
            }
        }
        ranked.sort_by(|a, b| b.0.strength.cmp(&a.0.strength).then(a.1.cmp(&b.1)));
        if let Some((winner, _, anchor)) = ranked.first() {
            // Treat nearby operation matches as uncertainty independently of
            // lexical scores, which are incomparable across different probes.
            let support: Vec<_> = ranked
                .iter()
                .skip(1)
                .filter(|(fit, _, _)| {
                    fit.strength + 4 >= winner.strength
                        || (fit.name_fit && fit.strength + 10 >= winner.strength)
                })
                .take(2)
                .map(|(_, _, anchor)| anchor.clone())
                .collect();
            let confidence = if !support.is_empty() || !winner.precise {
                "ambiguous"
            } else {
                "clear"
            };
            return Some(OperationRoute {
                anchor: anchor.clone(),
                support,
                confidence,
                reason: "matched the requested operation and object against production symbols",
            });
        }

        if hints.is_empty() {
            return None;
        }
        let reader = StoredSourceReader::within_checkout(&super::find_project_root(path)).ok()?;
        if let Some(command) = command {
            use aden_graph::cache::AnchorResolution;
            for prefix in ["cmd_", "run_"] {
                let name = format!("{prefix}{command}");
                if !index.knows_term(&name) {
                    continue;
                }
                if let Ok(AnchorResolution::Unique { anchor } | AnchorResolution::Exact(anchor)) =
                    session.resolve_anchor_exhaustive(&name)
                    && let Ok(Some(doc)) = session.document(&anchor)
                    && doc.node_type == NodeType::Function
                    && !related.iter().any(|(a, _)| *a == anchor)
                {
                    related.insert(0, (anchor, doc));
                    break;
                }
            }
        }
        let mut sources = HashMap::new();
        let mut handoffs = Vec::new();
        for (anchor, doc) in related {
            let Some(span) = SourceSpan::from_attributes(&doc.attributes) else {
                continue;
            };
            let Some(expected) = doc.attributes.get("source_hash") else {
                continue;
            };
            let file = PathBuf::from(&span.file);
            let source = sources.entry(file).or_insert_with_key(|file| {
                let source = reader.read(file).ok()?;
                Some((aden_core::hash_source(&source), source))
            });
            let Some((_, source)) = source.as_ref().filter(|(hash, _)| hash == expected) else {
                continue;
            };
            if span.start_line == 0 || span.start_line > span.end_line {
                continue;
            }
            let found: HashSet<_> = source
                .lines()
                .skip(span.start_line - 1)
                .take((span.end_line - span.start_line + 1).min(512))
                .filter(|line| !line.trim_start().starts_with("//"))
                .flat_map(contract_identifiers)
                .collect();
            let mut matches = std::collections::BTreeMap::new();
            for (hint, prose) in &hints {
                if found.contains(hint) {
                    *matches.entry(prose).or_insert(0) += 1;
                }
            }
            if let Some((prose, count)) = matches.into_iter().max_by_key(|(_, count)| *count)
                && count >= 2
            {
                handoffs.push((count, anchor, prose.clone()));
            }
        }
        handoffs.sort_by_key(|a| std::cmp::Reverse(a.0));
        let (_, anchor, prose) = handoffs.first()?;
        let mut support: Vec<_> = handoffs
            .iter()
            .skip(1)
            .take(1)
            .map(|(_, anchor, _)| anchor.clone())
            .collect();
        support.push(prose.clone());
        Some(OperationRoute {
            anchor: anchor.clone(),
            confidence: "ambiguous",
            support,
            reason: "matched identifiers from the documentation against verified source",
        })
    }
}

fn contract_identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter_map(|word| {
            let identifier = word.strip_prefix("--").unwrap_or(word);
            (identifier.contains('_') || word.starts_with("--"))
                .then_some(identifier.replace('-', "_"))
        })
        .filter(|word| word.len() >= 6)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_and_object_distinguish_symbols_on_the_same_topic() {
        let publish =
            Operation::parse("Which function publishes a prepared graph snapshot?").unwrap();
        let name = Operation::parse("Which function derives the graph snapshot filename?").unwrap();
        assert!(
            publish
                .fit(
                    "publish_prepared",
                    "Atomically publish the prepared snapshot",
                    "snapshot.rs",
                    &[],
                )
                .is_some()
        );
        assert!(
            publish
                .fit(
                    "graph_snapshot_file",
                    "Atomic read snapshot published at the end of a successful update",
                    "paths.rs",
                    &[],
                )
                .is_none()
        );
        assert!(
            name.fit(
                "graph_snapshot_file",
                "Atomic read snapshot published at the end of a successful update",
                "paths.rs",
                &[],
            )
            .is_some()
        );
        assert!(
            name.fit(
                "publish_prepared",
                "Atomically publish the prepared snapshot",
                "snapshot.rs",
                &[],
            )
            .is_none()
        );
    }

    #[test]
    fn named_operation_without_a_shared_object_does_not_match() {
        let route = Operation::parse("Which function verifies an archive checksum?").unwrap();
        assert!(
            route
                .fit("verify_request", "Verify caller identity", "auth.rs", &[])
                .is_none()
        );
        assert!(Operation::parse("How do I verify an archive checksum?").is_none());
    }

    #[test]
    fn passive_operation_and_named_definition_are_distinct() {
        let publication = Operation::parse("Where is the snapshot published?").unwrap();
        assert!(
            publication
                .fit("publish_snapshot", "", "storage.rs", &[])
                .is_some()
        );
        assert!(publication.fit("snapshot", "", "storage.rs", &[]).is_none());
        assert!(
            Operation::parse("Where is the ledger_snapshot published?")
                .unwrap()
                .passive
        );
        assert!(Operation::parse("Where is Snapshot defined?").is_none());
    }

    #[test]
    fn contract_identifiers_ignore_plain_mentions() {
        assert_eq!(
            contract_identifiers("Use `--byte-position` on `cursor_expired` with the cursor."),
            ["byte_position", "cursor_expired"]
        );
    }

    #[test]
    fn unlisted_verbs_are_taken_from_the_predicate() {
        let route =
            Operation::parse("Where does the billing service archive a refund invoice?").unwrap();
        assert!(
            route
                .fit("archive_invoice", "", "billing.rs", &[])
                .is_some()
        );
        assert!(route.fit("refund_invoice", "", "billing.rs", &[]).is_none());
        let plain = Operation::parse("Where does the billing service archive invoices?").unwrap();
        let qualified =
            Operation::parse("Where does the billing service archive unpaid invoices?").unwrap();
        assert!(
            plain
                .fit("archive_paid_invoice", "", "billing.rs", &[])
                .unwrap()
                .precise
        );
        assert!(
            !qualified
                .fit("archive_paid_invoice", "", "billing.rs", &[])
                .unwrap()
                .precise
        );
    }
}
