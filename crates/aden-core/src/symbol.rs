// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared natural-symbol normalization used by graph ranking and store indexes.

/// Natural forms derived from a canonical anchor's trailing segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NaturalAnchorForms {
    pub segment: String,
    pub segment_lower: String,
    pub leaf: String,
    pub leaf_lower: String,
    pub qualified_prefixes_lower: Vec<String>,
}

/// Normalize generic and whitespace-heavy human symbol spellings.
pub fn natural_symbol_form(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut generic_depth = 0usize;
    for ch in value.chars() {
        match ch {
            '<' => generic_depth += 1,
            '>' => generic_depth = generic_depth.saturating_sub(1),
            ch if generic_depth == 0 && !ch.is_whitespace() => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Extract pre-normalized forms needed by Aden's rank 0-5 resolution contract.
pub fn natural_anchor_forms(anchor: &str) -> NaturalAnchorForms {
    let segment = natural_symbol_form(anchor.rsplit(['#', '/']).next().unwrap_or(""));
    let segment_lower = segment.to_lowercase();
    let leaf = segment
        // `\\` is a language namespace separator (not a filesystem choice)
        // in PHP canonical symbols. Supporting it here makes shorthand lookup
        // independent of the host OS while canonical anchors remain URL-like.
        .rsplit(['.', ':', '\\'])
        .next()
        .unwrap_or(&segment)
        .to_string();
    let leaf_lower = leaf.to_lowercase();
    let mut qualified_prefixes_lower = Vec::new();
    let bytes = segment.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        let separator = *byte == b'.'
            || *byte == b'\\'
            || (*byte == b':' && bytes.get(index + 1) == Some(&b':'));
        if separator && index > 0 {
            let prefix = segment[..index].to_lowercase();
            if qualified_prefixes_lower.last() != Some(&prefix) {
                qualified_prefixes_lower.push(prefix);
            }
        }
    }
    NaturalAnchorForms {
        segment,
        segment_lower,
        leaf,
        leaf_lower,
        qualified_prefixes_lower,
    }
}

/// Maximum typo distance used for suggestion-only recovery.
pub fn typo_max_distance(symbol_len: usize) -> Option<usize> {
    match symbol_len {
        0..=2 => None,
        3..=4 => Some(1),
        _ => Some(2),
    }
}

/// Unicode-scalar Levenshtein distance for bounded typo recovery.
pub fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_char) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != *right_char);
            current[right_index + 1] = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_cover_generic_methods_and_all_qualified_prefixes() {
        let forms = natural_anchor_forms("aden://module/a.rs#Outer<T>::Inner<U>::run");
        assert_eq!(forms.segment, "Outer::Inner::run");
        assert_eq!(forms.leaf, "run");
        assert_eq!(forms.qualified_prefixes_lower, ["outer", "outer::inner"]);
        assert_eq!(natural_symbol_form("Outer < T > :: run"), "Outer::run");
    }

    #[test]
    fn forms_cover_php_namespace_shorthand_on_every_os() {
        let forms = natural_anchor_forms(
            r"aden://module/shop/src/Service.php#App\Billing\InvoiceService\send",
        );
        assert_eq!(forms.segment, r"App\Billing\InvoiceService\send");
        assert_eq!(forms.leaf, "send");
        assert_eq!(
            forms.qualified_prefixes_lower,
            ["app", r"app\billing", r"app\billing\invoiceservice"]
        );
    }

    #[test]
    fn typo_policy_is_bounded() {
        assert_eq!(typo_max_distance(2), None);
        assert_eq!(typo_max_distance(3), Some(1));
        assert_eq!(typo_max_distance(5), Some(2));
        assert_eq!(edit_distance("prase", "parse"), 2);
    }
}
