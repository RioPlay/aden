// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod audit;
pub mod ci;
pub mod communities;
pub mod complete;
pub mod config;
pub mod diagnose;
pub mod doctor;
pub mod emergency;
pub mod federation;
pub mod generate;
pub mod grep;
pub mod heal;
pub mod impact_diff;
pub mod init;
pub mod licenses;
pub mod lint;
pub mod locate;
#[cfg(feature = "model-fetch")]
pub mod model;
pub mod outcome;
pub mod overlay;
pub mod query;
pub mod ready;
pub mod review;

pub mod scope;
pub mod search;
pub mod session;
pub mod status;
pub mod store;
pub mod suggest;
pub mod sync;
pub mod test_cmd;
#[cfg(feature = "view")]
pub mod timeline;
#[cfg(feature = "view")]
pub mod view;
pub mod viz;

// Re-export all command functions so main.rs can use `commands::cmd_init(...)` etc.
pub use audit::cmd_audit;
pub use ci::cmd_ci_check;
pub use communities::cmd_communities;
pub use config::{cmd_config_get, cmd_config_set};
pub use diagnose::cmd_diagnose;
pub use doctor::cmd_doctor;
pub use emergency::cmd_emergency;
pub use federation::cmd_federation;
pub use generate::{
    StaleHintGuard, augment_read_json, cmd_gen, cmd_gen_opts, ensure_fresh, ensure_fresh_decision,
};
pub use grep::cmd_grep;
#[cfg(feature = "watch")]
pub use heal::cmd_heal_watch;
pub use heal::{cmd_heal_apply, cmd_heal_scan, cmd_heal_scan_since};
pub use impact_diff::cmd_impact_diff;
pub use init::{cmd_agents_md, cmd_init, cmd_new};
pub use licenses::cmd_licenses;
pub use lint::cmd_lint;
pub use locate::{cmd_locate, cmd_understand};
#[cfg(feature = "model-fetch")]
pub use model::cmd_model_fetch;
#[cfg(feature = "watch")]
pub use query::cmd_watch;
pub use query::{cmd_ask, cmd_asm, cmd_check, cmd_query, cmd_query_adq};
pub use ready::cmd_ready;
pub use review::{cmd_review, cmd_review_since};
pub use scope::{cmd_scope, cmd_scope_agents};
pub use search::{cmd_list, cmd_search};
pub use session::{cmd_kickoff, cmd_session, cmd_workflow};
pub use status::cmd_status;
pub use store::{cmd_store_list, cmd_store_migrate, cmd_store_path, cmd_store_prune};
pub use suggest::cmd_suggest;
pub use sync::cmd_sync;
pub use test_cmd::cmd_test;
#[cfg(feature = "view")]
pub use timeline::cmd_timeline;
#[cfg(feature = "view")]
pub use view::cmd_view;
pub use viz::cmd_viz;
