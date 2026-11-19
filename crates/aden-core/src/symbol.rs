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

/// The eight closest symbol suggestions, ordered by distance then canonical anchor.
///
/// Multiple occurrences of an anchor with the same distance share one slot.
#[derive(Default)]
pub struct SymbolSuggestions {
    entries: Vec<(usize, String)>,
}

impl SymbolSuggestions {
    const LIMIT: usize = 8;

    pub fn insert(&mut self, distance: usize, anchor: &str) {
        let rank = (distance, anchor);
        let Err(index) = self
            .entries
            .binary_search_by(|(distance, anchor)| (*distance, anchor.as_str()).cmp(&rank))
        else {
            return;
        };
        if index >= Self::LIMIT {
            return;
        }
        if self.entries.len() == Self::LIMIT {
            self.entries.pop();
        }
        self.entries.insert(index, (distance, anchor.to_owned()));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn into_anchors(self) -> Vec<String> {
        self.entries.into_iter().map(|(_, anchor)| anchor).collect()
    }
}

/// Exact Unicode-scalar Levenshtein distance.
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

/// Exact Unicode-scalar Levenshtein distance up to `max_distance`.
///
/// The typo policy's limits use a fixed-size band. Larger limits use the
/// exact distance routine.
pub fn edit_distance_within(left: &str, right: &str, max_distance: usize) -> Option<usize> {
    const MAX_BAND_DISTANCE: usize = 2;
    const ROW_WIDTH: usize = 2 * MAX_BAND_DISTANCE + 3;

    if left == right {
        return Some(0);
    }
    if max_distance == 0 {
        return None;
    }

    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len.abs_diff(right_len) > max_distance {
        return None;
    }
    if left_len == 0 || right_len == 0 {
        return Some(left_len.max(right_len));
    }
    if max_distance > MAX_BAND_DISTANCE {
        let distance = edit_distance(left, right);
        return (distance <= max_distance).then_some(distance);
    }

    // Keep only the diagonals that can reach the target within the limit,
    // with one sentinel for a missing predecessor on either side.
    let unreachable = max_distance + 1;
    let center = max_distance + 1;
    let mut previous_row = [unreachable; ROW_WIDTH];
    let mut current_row = [unreachable; ROW_WIDTH];
    let (mut previous, mut current) = (&mut previous_row, &mut current_row);
    for column in 0..=right_len.min(max_distance) {
        previous[center + column] = column;
    }

    // Slide the right-hand iterator with the band. Cloning a Chars iterator
    // walks just this row's window without allocating a vector of scalars.
    let mut right_window = right.chars();
    for (index, left_char) in left.chars().enumerate() {
        let row = index + 1;
        let first_column = row.saturating_sub(max_distance);
        let last_column = (row + max_distance).min(right_len);
        let first_cell = center - (row - first_column);
        let last_cell = first_cell + (last_column - first_column);
        current[first_cell - 1] = unreachable;
        current[last_cell + 1] = unreachable;

        let mut first_char_cell = first_cell;
        let mut row_min = unreachable;
        if first_column == 0 {
            current[first_cell] = row;
            first_char_cell += 1;
            row_min = row;
        }
        for (cell, right_char) in (first_char_cell..=last_cell).zip(right_window.clone()) {
            let substitution = previous[cell] + usize::from(left_char != right_char);
            let distance = (current[cell - 1] + 1)
                .min(previous[cell + 1] + 1)
                .min(substitution);
            current[cell] = distance;
            row_min = row_min.min(distance);
        }
        if row_min > max_distance {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
        if first_column >= 1 {
            right_window.next();
        }
    }
    let distance = previous[center + right_len - left_len];
    (distance <= max_distance).then_some(distance)
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

    #[test]
    fn symbol_suggestions_match_full_sort_and_dedup() {
        let candidates = [
            (2, "example#zed"),
            (1, "example#west"),
            (2, "example#north"),
            (1, "example#east"),
            (2, "example#south"),
            (1, "example#bay"),
            (2, "example#westward"),
            (1, "example#rift"),
            (1, "example#east"),
            (1, "example#hill"),
            (2, "example#zed"),
            (1, "example#cove"),
            (2, "example#vale"),
            (2, "example#north"),
        ];
        let mut suggestions = SymbolSuggestions::default();
        assert!(suggestions.is_empty());
        for (index, &(distance, anchor)) in candidates.iter().enumerate() {
            suggestions.insert(distance, anchor);
            assert!(!suggestions.is_empty());
            assert!(suggestions.entries.len() <= 8);

            let mut expected = candidates[..=index].to_vec();
            expected.sort_unstable();
            expected.dedup_by_key(|(_, anchor)| *anchor);
            expected.truncate(8);
            let actual: Vec<_> = suggestions
                .entries
                .iter()
                .map(|(distance, anchor)| (*distance, anchor.as_str()))
                .collect();
            assert_eq!(actual, expected);
        }
        assert_eq!(
            suggestions.into_anchors(),
            [
                "example#bay",
                "example#cove",
                "example#east",
                "example#hill",
                "example#rift",
                "example#west",
                "example#north",
                "example#south",
            ]
        );
    }

    #[test]
    fn bounded_edit_distance_matches_exact_for_short_unicode_strings() {
        let mut strings = vec![String::new()];
        let mut start = 0;
        for _ in 0..4 {
            let end = strings.len();
            for index in start..end {
                for suffix in ['a', 'é', '🙂'] {
                    let mut value = strings[index].clone();
                    value.push(suffix);
                    strings.push(value);
                }
            }
            start = end;
        }

        for left in &strings {
            for right in &strings {
                let exact = edit_distance(left, right);
                for limit in [0, 1, 2, 3, 4, 5, usize::MAX] {
                    assert_eq!(
                        edit_distance_within(left, right, limit),
                        (exact <= limit).then_some(exact),
                        "{left:?} vs. {right:?}, limit {limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn bounded_edit_distance_handles_long_unicode_matches_and_rejections() {
        let prefix = "界".repeat(4096);
        let left = format!("{prefix}a🙂é");
        let right = format!("{prefix}🙂à");
        assert_eq!(edit_distance_within(&left, &right, 2), Some(2));
        assert_eq!(edit_distance_within(&right, &left, 2), Some(2));
        assert_eq!(edit_distance_within(&left, &right, 1), None);
        assert_eq!(
            edit_distance_within(&format!("abc{prefix}"), &format!("xyz{prefix}"), 2),
            None
        );
        assert_eq!(edit_distance_within("é", "e\u{301}", 1), None);
        assert_eq!(edit_distance_within("é", "e\u{301}", 2), Some(2));
    }
}
