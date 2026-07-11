// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Stable, additive result semantics shared by Aden's validation commands.
//!
//! `outcome` answers whether a command may be trusted/acted on. `result_state`
//! describes the shape of the returned data. They are deliberately independent:
//! an empty, fully evaluated result can be clean, while a partial result is
//! incomplete even when no blocker was observed in the portion that ran.

use serde::Serialize;

#[allow(dead_code)] // Partial/error/ambiguous are schema commitments used as commands adopt receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Clean,
    PassedWithFindings,
    Blocked,
    Degraded,
    Incomplete,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultState {
    Empty,
    Complete,
    Partial,
    Ambiguous,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct OutcomeEnvelope {
    pub schema_version: u8,
    pub outcome: Outcome,
    pub result_state: ResultState,
    pub graph_health: &'static str,
    pub policy_outcome: &'static str,
    pub freshness_outcome: &'static str,
    pub blocking_findings: usize,
    pub advisory_findings: usize,
    pub complete: bool,
}

impl OutcomeEnvelope {
    pub fn evaluated(
        blocking_findings: usize,
        advisory_findings: usize,
        graph_health: &'static str,
        policy_outcome: &'static str,
        freshness_outcome: &'static str,
    ) -> Self {
        let outcome = if blocking_findings > 0 {
            Outcome::Blocked
        } else if graph_health == "degraded"
            || policy_outcome == "degraded"
            || freshness_outcome == "degraded"
        {
            Outcome::Degraded
        } else if advisory_findings > 0 {
            Outcome::PassedWithFindings
        } else {
            Outcome::Clean
        };
        Self {
            schema_version: 1,
            outcome,
            result_state: if blocking_findings + advisory_findings == 0 {
                ResultState::Empty
            } else {
                ResultState::Complete
            },
            graph_health,
            policy_outcome,
            freshness_outcome,
            blocking_findings,
            advisory_findings,
            complete: true,
        }
    }

    #[allow(dead_code)]
    pub fn incomplete(result_state: ResultState) -> Self {
        debug_assert!(matches!(
            result_state,
            ResultState::Partial | ResultState::Error
        ));
        Self {
            schema_version: 1,
            outcome: Outcome::Incomplete,
            result_state,
            graph_health: "unknown",
            policy_outcome: "unknown",
            freshness_outcome: "unknown",
            blocking_findings: 0,
            advisory_findings: 0,
            complete: false,
        }
    }

    #[allow(dead_code)]
    pub fn ambiguous(advisory_findings: usize) -> Self {
        let mut value = Self::evaluated(0, advisory_findings, "healthy", "advisory", "fresh");
        value.result_state = ResultState::Ambiguous;
        value
    }
}

pub fn policy_label(violations: usize, unwired: bool) -> &'static str {
    if unwired {
        "degraded"
    } else if violations > 0 {
        "advisory_findings"
    } else {
        "clean"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_classes_are_independent_from_result_shape() {
        assert_eq!(
            OutcomeEnvelope::evaluated(0, 0, "healthy", "clean", "fresh").outcome,
            Outcome::Clean
        );
        assert_eq!(
            OutcomeEnvelope::evaluated(0, 2, "healthy", "advisory_findings", "fresh").outcome,
            Outcome::PassedWithFindings
        );
        assert_eq!(
            OutcomeEnvelope::evaluated(1, 0, "unhealthy", "clean", "fresh").outcome,
            Outcome::Blocked
        );
        assert_eq!(
            OutcomeEnvelope::evaluated(0, 0, "degraded", "clean", "fresh").outcome,
            Outcome::Degraded
        );
        assert_eq!(
            OutcomeEnvelope::incomplete(ResultState::Partial).outcome,
            Outcome::Incomplete
        );
        assert_eq!(
            OutcomeEnvelope::ambiguous(2).result_state,
            ResultState::Ambiguous
        );
    }
}
