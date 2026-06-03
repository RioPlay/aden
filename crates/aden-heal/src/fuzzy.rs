// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub fn suggest_anchor(query: &str, candidates: &[String]) -> Option<(String, f64)> {
    let mut best: Option<(String, f64)> = None;

    for candidate in candidates {
        let dist = levenshtein(query, candidate);
        let max_len = query.chars().count().max(candidate.chars().count());
        if max_len == 0 {
            continue;
        }
        let confidence = 1.0 - (dist as f64 / max_len as f64);
        if confidence >= 0.85 {
            if let Some((_, current_best_conf)) = best {
                if confidence > current_best_conf {
                    best = Some((candidate.clone(), confidence));
                }
            } else {
                best = Some((candidate.clone(), confidence));
            }
        }
    }

    best
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev = vec![0; n + 1];
    let mut curr = vec![0; n + 1];

    for (j, item) in prev.iter_mut().enumerate() {
        *item = j;
    }

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}
