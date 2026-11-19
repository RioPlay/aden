// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

/// Bound simultaneously retained parse results by both source bytes and file
/// count. A source larger than the byte target runs alone: parsers operate on
/// whole files, so this is a batching target, not a hard process memory quota.
pub(super) const SOURCE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const SOURCE_FILES: usize = 32;

#[cfg(test)]
fn batch_len(sizes: impl IntoIterator<Item = u64>) -> (usize, u64) {
    batch_len_with_limits(sizes, SOURCE_FILES, SOURCE_BYTES)
}

pub(super) fn batch_len_with_limits(
    sizes: impl IntoIterator<Item = u64>,
    max_files: usize,
    max_bytes: u64,
) -> (usize, u64) {
    let mut count = 0;
    let mut bytes = 0_u64;
    for size in sizes {
        if count > 0 && (count == max_files || size > max_bytes.saturating_sub(bytes)) {
            break;
        }
        count += 1;
        bytes = bytes.saturating_add(size);
    }
    (count, bytes)
}

pub(super) fn limits() -> (usize, u64) {
    #[cfg(test)]
    {
        TEST_LIMITS.with(|value| value.get())
    }
    #[cfg(not(test))]
    (SOURCE_FILES, SOURCE_BYTES)
}

#[cfg(test)]
thread_local! {
    static TEST_LIMITS: std::cell::Cell<(usize, u64)> = const {
        std::cell::Cell::new((SOURCE_FILES, SOURCE_BYTES))
    };
}

#[cfg(test)]
pub(super) fn with_limits<T>(limits: (usize, u64), run: impl FnOnce() -> T) -> T {
    struct Reset((usize, u64));
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_LIMITS.with(|value| value.set(self.0));
        }
    }
    let _reset = Reset(TEST_LIMITS.with(|value| value.replace(limits)));
    run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_override_restores_default_on_success_and_panic() {
        let default = limits();
        with_limits((1, 2), || assert_eq!(limits(), (1, 2)));
        assert_eq!(limits(), default);
        assert!(
            std::panic::catch_unwind(|| with_limits((3, 4), || panic!("test unwind"))).is_err()
        );
        assert_eq!(limits(), default);
    }

    #[test]
    fn batches_accept_exact_byte_target_and_reject_next_file() {
        assert_eq!(
            batch_len([SOURCE_BYTES / 2, SOURCE_BYTES / 2, 1]),
            (2, SOURCE_BYTES)
        );
        assert_eq!(batch_len([SOURCE_BYTES, 1]), (1, SOURCE_BYTES));
    }

    #[test]
    fn file_limit_bounds_empty_and_tiny_sources() {
        assert_eq!(batch_len([0; SOURCE_FILES + 1]), (SOURCE_FILES, 0));
        assert_eq!(
            batch_len([1; SOURCE_FILES + 1]),
            (SOURCE_FILES, SOURCE_FILES as u64)
        );
        assert_eq!(batch_len([]), (0, 0));
    }

    #[test]
    fn oversized_file_makes_progress_alone_without_overflow() {
        assert_eq!(batch_len([u64::MAX, 1]), (1, u64::MAX));
        assert_eq!(batch_len([1, u64::MAX]), (1, 1));
    }

    #[test]
    fn bounded_drain_preserves_order_and_collision_winner_with_less_staged_payload() {
        // Deterministic parse-result stand-ins include a collision spanning
        // batches. The unit-only all-at-once policy cannot be selected by CLI.
        let files: Vec<_> = (0..97)
            .map(|i| (format!("symbol_{}", i % 31), format!("body_{i:04}")))
            .collect();
        let drain = |max_files, max_bytes| {
            let mut start = 0;
            let mut peak = 0;
            let mut ordered = Vec::new();
            let mut stored = std::collections::BTreeMap::new();
            while start < files.len() {
                let (len, _) = batch_len_with_limits(
                    files[start..].iter().map(|(_, text)| text.len() as u64),
                    max_files,
                    max_bytes,
                );
                let staged = files[start..start + len].to_vec();
                peak = peak.max(staged.iter().map(|(_, text)| text.len()).sum::<usize>());
                for (anchor, text) in staged {
                    ordered.push((anchor.clone(), text.clone()));
                    stored.insert(anchor, text);
                }
                start += len;
            }
            (ordered, stored, peak)
        };
        let bounded = drain(SOURCE_FILES, SOURCE_BYTES);
        let legacy = drain(usize::MAX, u64::MAX);
        assert_eq!(bounded.0, legacy.0);
        assert_eq!(bounded.1, legacy.1);
        assert!(bounded.2 < legacy.2);
    }
}
