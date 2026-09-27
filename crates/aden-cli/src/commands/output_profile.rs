// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

use clap::ValueEnum;

/// Versioned response shapes for commands that support token-lean output.
///
/// `Full` preserves the established machine-readable contract. New compact
/// profiles are opt-in so scripts can migrate deliberately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputProfile {
    #[default]
    Full,
    CompactV2,
}

impl OutputProfile {
    pub fn is_compact(self) -> bool {
        matches!(self, Self::CompactV2)
    }
}
