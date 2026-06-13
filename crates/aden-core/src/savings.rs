// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic, zero-inference estimate of the tokens — and dollars — saved by
//! answering a query from the graph instead of having an agent grep-and-read full
//! source files.
//!
//! Everything here is pure arithmetic on data Aden already holds at emit time, so
//! computing it costs **no** LLM call. It is an *estimate*, against an explicit
//! baseline model ("grep-and-read up to [`BASELINE_MAX_FILES`] full source
//! files", matching `docs/benchmarks.adoc`) and the same ~4-bytes/token heuristic
//! the assembler budgets by. Surfaces must tag the figure `[est.]` and never
//! present it as measured.

/// Published Anthropic input price per 1M tokens, by tier (as of 2026-06).
///
/// Output tokens bill higher (5×), but the savings measured here are *context*
/// (input) tokens the agent would otherwise load — so the input rate is the
/// honest, conservative choice. Displaced reasoning/output is not counted here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PriceTier {
    /// `claude-fable-5` — $10 / 1M input.
    Fable5,
    /// `claude-opus-4-8` — $5 / 1M input. Default.
    #[default]
    Opus48,
    /// `claude-sonnet-4-6` — $3 / 1M input.
    Sonnet46,
    /// `claude-haiku-4-5` — $1 / 1M input.
    Haiku45,
}

impl PriceTier {
    /// Published input price in USD per 1,000,000 tokens.
    pub fn input_usd_per_mtok(self) -> f64 {
        match self {
            PriceTier::Fable5 => 10.0,
            PriceTier::Opus48 => 5.0,
            PriceTier::Sonnet46 => 3.0,
            PriceTier::Haiku45 => 1.0,
        }
    }

    /// Canonical model id string for this tier.
    pub fn id(self) -> &'static str {
        match self {
            PriceTier::Fable5 => "claude-fable-5",
            PriceTier::Opus48 => "claude-opus-4-8",
            PriceTier::Sonnet46 => "claude-sonnet-4-6",
            PriceTier::Haiku45 => "claude-haiku-4-5",
        }
    }

    /// Resolve from a config string. Accepts the canonical model id or a short
    /// alias (`fable5`, `opus48`, `sonnet46`, `haiku45`). Case-insensitive.
    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-fable-5" | "fable5" | "fable" => Some(PriceTier::Fable5),
            "claude-opus-4-8" | "opus48" | "opus" => Some(PriceTier::Opus48),
            "claude-sonnet-4-6" | "sonnet46" | "sonnet" => Some(PriceTier::Sonnet46),
            "claude-haiku-4-5" | "haiku45" | "haiku" => Some(PriceTier::Haiku45),
            _ => None,
        }
    }
}

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
    /// Pricing tier the dollar figure was computed against.
    pub tier: PriceTier,
    /// `saved_tokens` priced at the tier's input rate; may be negative.
    pub saved_usd: f64,
}

impl SavingsEstimate {
    /// Build an estimate from raw byte counts.
    ///
    /// * `returned_bytes` — bytes Aden emitted for this query.
    /// * `baseline_bytes` — total on-disk bytes of the (already capped) source
    ///   files an agent would have read to answer the same question.
    /// * `baseline_files` — how many files those bytes span (for the label).
    pub fn from_bytes(
        returned_bytes: usize,
        baseline_bytes: usize,
        baseline_files: usize,
        tier: PriceTier,
    ) -> Self {
        let returned_tokens = tokens_from_bytes(returned_bytes);
        let baseline_tokens = tokens_from_bytes(baseline_bytes);
        let saved_tokens = baseline_tokens as i64 - returned_tokens as i64;
        let saved_usd = (saved_tokens as f64 / 1_000_000.0) * tier.input_usd_per_mtok();
        Self {
            returned_tokens,
            baseline_tokens,
            baseline_files,
            saved_tokens,
            tier,
            saved_usd,
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
    /// the baseline, the tier, and the `[est.]` tag inline.
    pub fn footer_line(&self) -> String {
        format!(
            "//   Savings : ~{} tok / ~${:.4} vs grep-read {} file(s) [est., {}]",
            self.saved_tokens,
            self.saved_usd,
            self.baseline_files,
            self.tier.id()
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
    fn positive_savings_priced_at_input_rate() {
        // 40 KB baseline, 4 KB returned → 10k vs 1k tokens, 9k saved.
        let e = SavingsEstimate::from_bytes(4_000, 40_000, 3, PriceTier::Opus48);
        assert_eq!(e.returned_tokens, 1_000);
        assert_eq!(e.baseline_tokens, 10_000);
        assert_eq!(e.saved_tokens, 9_000);
        // 9_000 / 1e6 * $5 = $0.045
        assert!((e.saved_usd - 0.045).abs() < 1e-9);
    }

    #[test]
    fn negative_savings_are_reported_not_clamped() {
        // Assembled neighborhood larger than a single small baseline file.
        let e = SavingsEstimate::from_bytes(20_000, 4_000, 1, PriceTier::Opus48);
        assert_eq!(e.saved_tokens, 1_000 - 5_000);
        assert!(e.saved_usd < 0.0);
    }

    #[test]
    fn tier_changes_dollars_not_tokens() {
        let opus = SavingsEstimate::from_bytes(4_000, 40_000, 3, PriceTier::Opus48);
        let haiku = SavingsEstimate::from_bytes(4_000, 40_000, 3, PriceTier::Haiku45);
        assert_eq!(opus.saved_tokens, haiku.saved_tokens);
        // Opus input is 5× a Haiku input dollar.
        assert!((opus.saved_usd - haiku.saved_usd * 5.0).abs() < 1e-9);
    }

    #[test]
    fn tool_calls_saved_equals_baseline_files() {
        let e = SavingsEstimate::from_bytes(4_000, 40_000, 3, PriceTier::Opus48);
        assert_eq!(e.tool_calls_saved(), 3);
    }

    #[test]
    fn humanize() {
        assert_eq!(humanize_count(937), "937");
        assert_eq!(humanize_count(42_455), "42k");
        assert_eq!(humanize_count(2_900_000), "2.9M");
        assert_eq!(humanize_count(-5_000), "-5k");
    }

    #[test]
    fn tier_id_roundtrips() {
        for t in [
            PriceTier::Fable5,
            PriceTier::Opus48,
            PriceTier::Sonnet46,
            PriceTier::Haiku45,
        ] {
            assert_eq!(PriceTier::from_id(t.id()), Some(t));
        }
        assert_eq!(PriceTier::from_id("OPUS"), Some(PriceTier::Opus48));
        assert_eq!(PriceTier::from_id("nope"), None);
        assert_eq!(PriceTier::default(), PriceTier::Opus48);
    }
}
