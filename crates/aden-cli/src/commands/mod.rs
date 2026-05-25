pub mod generate;
pub mod heal;
pub mod init;
pub mod misc;
pub mod query;
pub mod session;

// Re-export all command functions so main.rs can use `commands::cmd_init(...)` etc.
pub use generate::cmd_gen;
pub use heal::{cmd_heal_apply, cmd_heal_scan, cmd_heal_scan_since};
pub use init::{cmd_init, cmd_new};
pub use misc::{cmd_audit, cmd_ci_check, cmd_doctor, cmd_emergency, cmd_licenses, cmd_review, cmd_review_since};
pub use query::{cmd_ask, cmd_asm, cmd_check, cmd_graph, cmd_locate, cmd_query, cmd_query_adq, cmd_search};
#[cfg(feature = "watch")]
pub use query::cmd_watch;
#[cfg(feature = "watch")]
pub use heal::cmd_heal_watch;
pub use session::{cmd_kickoff, cmd_session, cmd_workflow};
