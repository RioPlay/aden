// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic, zero-inference estimate of the **tokens** saved by answering a
//! query from the graph instead of having an agent grep-and-read full source
//! files.
//!
//! Tokens are the only unit reported, on purpose: a dollar figure would have to
//! assume one model's price (and prices drift), which makes it dishonest the
//! moment a different model — or a new price — is in play. Tokens are the
//! model-independent quantity Aden actually displaces, so that is what we track.
//!
//! Everything here is pure arithmetic on data Aden already holds at emit time, so
//! computing it costs **no** LLM call. It is an *estimate*, against an explicit
//! baseline model ("grep-and-read up to [`BASELINE_MAX_FILES`] full source
//! files", matching `docs/benchmarks.adoc`) and the same ~4-bytes/token heuristic
//! the assembler budgets by. Surfaces must tag the figure `[est.]` and never
//! present it as measured.

/// The published baseline: a grep-and-read agent opens up to this many full
/// source files per query (matches `docs/benchmarks.adoc` methodology).
pub const BASELINE_MAX_FILES: usize = 5;

/// Tokens for a byte count, using the ~4-bytes/token heuristic the assembler and
/// the `ask` summary footer both use, so every surface compares like with like.
pub fn tokens_from_bytes(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// A single-query savings estimate. `saved_tokens` is signed: a large assembled
/// neighborhood can legitimately exceed a small baseline, in which case the
/// honest report is a negative number, not a clamp to zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavingsEstimate {
    /// Tokens Aden actually emitted (what the agent consumes).
    pub returned_tokens: usize,
    /// Tokens a grep-and-read agent would have loaded (the baseline).
    pub baseline_tokens: usize,
    /// Number of baseline source files counted (capped at [`BASELINE_MAX_FILES`]).
    pub baseline_files: usize,
    /// `baseline_tokens - returned_tokens`; may be negative.
    pub saved_tokens: i64,
}

impl SavingsEstimate {
    /// Build an estimate from raw byte counts.
    ///
    /// * `returned_bytes` — bytes Aden emitted for this query.
    /// * `baseline_bytes` — total on-disk bytes of the (already capped) source
    ///   files an agent would have read to answer the same question.
    /// * `baseline_files` — how many files those bytes span (for the label).
    pub fn from_bytes(returned_bytes: usize, baseline_bytes: usize, baseline_files: usize) -> Self {
        let returned_tokens = tokens_from_bytes(returned_bytes);
        let baseline_tokens = tokens_from_bytes(baseline_bytes);
        let saved_tokens = baseline_tokens as i64 - returned_tokens as i64;
        Self {
            returned_tokens,
            baseline_tokens,
            baseline_files,
            saved_tokens,
        }
    }

    /// Estimated agent tool calls displaced by serving this query from the graph.
    ///
    /// Baseline behavior model (matches `docs/benchmarks.adoc`): a grep-and-read
    /// agent answers one question with `1` search/grep call + `N` file-read calls,
    /// where `N` is [`Self::baseline_files`]. With Aden it is a single tool call.
    /// The grep and the Aden call cancel 1:1, so the net saving is `N`:
    ///
    /// ```text
    /// (1 grep + N reads) - (1 aden call) = N = baseline_files
    /// ```
    ///
    /// This is an estimate of what an agent would *otherwise* do, not a count of
    /// what it did — surface it `[est.]`. The third-party "2.1× fewer tool calls"
    /// figure is a conservative cross-check, not the source of this number.
    pub fn tool_calls_saved(&self) -> usize {
        self.baseline_files
    }

    /// Terse one-line footer for CLI surfaces. Honest and self-describing: states
    /// the unit, the baseline, and the `[est.]` tag inline.
    pub fn footer_line(&self) -> String {
        format!(
            "//   Savings : ~{} tokens saved vs grep-read {} file(s) [est.]",
            self.saved_tokens, self.baseline_files,
        )
    }
}

/// Compact human-readable count for summary lines: `2_900_000 -> "2.9M"`,
/// `42_455 -> "42k"`, `937 -> "937"`. Negative values keep their sign.
pub fn humanize_count(n: i64) -> String {
    let abs = n.unsigned_abs();
    let sign = if n < 0 { "-" } else { "" };
    if abs >= 1_000_000 {
        format!("{sign}{:.1}M", abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{sign}{}k", abs / 1_000)
    } else {
        format!("{sign}{abs}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_up() {
        assert_eq!(tokens_from_bytes(0), 0);
        assert_eq!(tokens_from_bytes(1), 1);
        assert_eq!(tokens_from_bytes(4), 1);
        assert_eq!(tokens_from_bytes(5), 2);
    }

    #[test]
    fn positive_savings_in_tokens() {
        // 40 KB baseline, 4 KB returned → 10k vs 1k tokens, 9k saved.
        let e = SavingsEstimate::from_bytes(4_000, 40_000, 3);
        assert_eq!(e.returned_tokens, 1_000);
        assert_eq!(e.baseline_tokens, 10_000);
        assert_eq!(e.saved_tokens, 9_000);
    }

    #[test]
    fn negative_savings_are_reported_not_clamped() {
        // Assembled neighborhood larger than a single small baseline file.
        let e = SavingsEstimate::from_bytes(20_000, 4_000, 1);
        assert_eq!(e.saved_tokens, 1_000 - 5_000);
    }

    #[test]
    fn footer_is_token_only() {
        let e = SavingsEstimate::from_bytes(4_000, 40_000, 3);
        let line = e.footer_line();
        assert!(line.contains("9000 tokens saved"), "got: {line}");
        assert!(
            !line.contains('$'),
            "footer must carry no dollar figure: {line}"
        );
    }

    #[test]
    fn tool_calls_saved_equals_baseline_files() {
        let e = SavingsEstimate::from_bytes(4_000, 40_000, 3);
        assert_eq!(e.tool_calls_saved(), 3);
    }

    #[test]
    fn humanize() {
        assert_eq!(humanize_count(937), "937");
        assert_eq!(humanize_count(42_455), "42k");
        assert_eq!(humanize_count(2_900_000), "2.9M");
        assert_eq!(humanize_count(-5_000), "-5k");
    }
}
