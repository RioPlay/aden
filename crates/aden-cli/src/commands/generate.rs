// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Thin wrapper around the indexer — public API for commands and heal/query.

#![allow(unused_imports)]

pub use crate::indexer::fresh::{
    STALE_HINT, StaleHintGuard, ensure_fresh, index_is_stale, maybe_print_stale_hint,
};
pub use crate::indexer::r#gen::{cmd_gen, cmd_gen_opts, cmd_gen_silent};

pub(crate) use crate::indexer::fresh::{recover_if_incompatible_store, skip_auto_gen_on_read};
pub(crate) use crate::indexer::link::doc_anchor_file;
pub(crate) use crate::indexer::merge::slim_doc_for_store;
