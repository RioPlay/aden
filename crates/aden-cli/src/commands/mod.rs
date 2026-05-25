pub mod federation;
pub mod generate;
pub mod heal;
pub mod init;
pub mod lint;
pub mod misc;
pub mod query;
pub mod session;
pub mod test_cmd;

// Re-export all command functions so main.rs can use `commands::cmd_init(...)` etc.
pub use federation::cmd_federation;
pub use generate::cmd_gen;
#[cfg(feature = "watch")]
pub use heal::cmd_heal_watch;
pub use heal::{cmd_heal_apply, cmd_heal_scan, cmd_heal_scan_since};
pub use init::{cmd_init, cmd_new};
pub use lint::cmd_lint;
pub use misc::{
    cmd_audit, cmd_ci_check, cmd_doctor, cmd_emergency, cmd_licenses, cmd_review, cmd_review_since,
    cmd_suggest,
};
#[cfg(feature = "watch")]
pub use query::cmd_watch;
pub use query::{
    cmd_ask, cmd_asm, cmd_check, cmd_list, cmd_locate, cmd_query, cmd_query_adq, cmd_search,
};
pub use session::{cmd_kickoff, cmd_session, cmd_workflow};
pub use test_cmd::cmd_test;
