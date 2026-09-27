// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
pub(crate) use aden_mcp::navigation::{inspect, search, search_results_page, tree};

pub(crate) fn print(actions: &[serde_json::Value]) {
    for action in actions {
        if let (Some(reason), Some(command)) =
            (action["reason"].as_str(), action["command"].as_str())
        {
            println!(
                "Next: {}\n  {}",
                crate::util::sanitize_terminal(reason),
                crate::util::sanitize_terminal(command)
            );
        }
    }
}
