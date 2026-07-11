// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! std-only UTC datetime formatting — no `chrono` dependency.
//!
//! aden only ever needs to stamp "now" (and "now + ttl") as UTC strings for the
//! `viz` export header and the emergency-override audit log. `chrono` pulled
//! `iana-time-zone`/`js-sys`/`num-traits` just for that, so these helpers replace
//! it with the same civil-date algorithm the `timeline` command already uses
//! (Howard Hinnant's `civil_from_days`), extended with time-of-day. UTC only.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as whole seconds since the Unix epoch.
pub(crate) fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `(year, month, day)` for the UTC day containing `secs`, via Howard Hinnant's
/// `civil_from_days` (<http://howardhinnant.github.io/date_algorithms.html>).
fn civil_from_secs(secs: u64) -> (i64, u64, u64) {
    let z = (secs / 86_400) as i64 + 719_468_i64; // days since 0000-03-01
    let era: i64 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // day of era [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month, March-based [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// `(hour, minute, second)` within the UTC day containing `secs`.
fn time_of_day(secs: u64) -> (u64, u64, u64) {
    let s = secs % 86_400;
    (s / 3_600, (s % 3_600) / 60, s % 60)
}

/// Format Unix seconds as RFC 3339 UTC: `YYYY-MM-DDTHH:MM:SSZ` (literal `Z`, no
/// sub-second precision).
pub(crate) fn unix_secs_to_rfc3339(secs: u64) -> String {
    let (y, mo, d) = civil_from_secs(secs);
    let (h, mi, s) = time_of_day(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Format Unix seconds as a compact, filename-safe stamp: `YYYYMMDD-HHMMSS`.
pub(crate) fn unix_secs_to_compact(secs: u64) -> String {
    let (y, mo, d) = civil_from_secs(secs);
    let (h, mi, s) = time_of_day(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(unix_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_secs_to_compact(0), "19700101-000000");
    }

    #[test]
    fn known_instant_with_time_of_day() {
        // 2021-01-01T00:00:00Z = 1_609_459_200, plus 13:37:42 (49_062 s).
        let base = 1_609_459_200;
        assert_eq!(unix_secs_to_rfc3339(base), "2021-01-01T00:00:00Z");
        assert_eq!(unix_secs_to_rfc3339(base + 49_062), "2021-01-01T13:37:42Z");
        assert_eq!(unix_secs_to_compact(base + 49_062), "20210101-133742");
    }

    #[test]
    fn leap_day() {
        // 2024-02-29T12:00:00Z = 1_709_208_000
        assert_eq!(unix_secs_to_rfc3339(1_709_208_000), "2024-02-29T12:00:00Z");
    }
}
