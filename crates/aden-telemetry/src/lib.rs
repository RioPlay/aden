// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Telemetry module for Aden: OTLP ingestion, guard firing events, latency analysis.
//!
//! Phase 5 of the Aden roadmap.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub event_type: EventType,
    pub timestamp: String,
    pub contract_id: Option<String>,
    pub guard_name: Option<String>,
    pub latency_ms: Option<u64>,
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventType {
    GuardFired,
    GuardBlocked,
    LatencySpike,
    OverrideUsed,
    DirectiveUpdated,
    AgentConfidenceUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardTelemetry {
    pub guard_name: String,
    pub fire_count: u64,
    pub block_count: u64,
    pub last_fired: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTelemetry {
    pub agent_id: String,
    pub directive_count: u64,
    pub success_count: u64,
    pub confidence: f64,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyReport {
    pub contract_id: String,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: u64,
    pub p95_latency_ms: u64,
    pub p99_latency_ms: u64,
    pub sample_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSavingsReport {
    pub baseline_tokens: usize,
    pub aden_tokens: usize,
    pub savings_percent: f64,
    pub period: String,
}

pub fn emit_guard_telemetry(guard_name: &str, fired: bool) -> TelemetryEvent {
    TelemetryEvent {
        event_type: if fired { EventType::GuardFired } else { EventType::GuardBlocked },
        timestamp: rfc3339_now(),
        contract_id: None,
        guard_name: Some(guard_name.to_string()),
        latency_ms: None,
        metadata: HashMap::new(),
    }
}

pub fn emit_latency_spike(contract_id: &str, latency_ms: u64) -> TelemetryEvent {
    TelemetryEvent {
        event_type: EventType::LatencySpike,
        timestamp: rfc3339_now(),
        contract_id: Some(contract_id.to_string()),
        guard_name: None,
        latency_ms: Some(latency_ms),
        metadata: HashMap::new(),
    }
}

pub fn compute_token_savings(baseline: usize, aden_tokens: usize) -> TokenSavingsReport {
    let savings = if baseline > 0 {
        ((baseline - aden_tokens) as f64 / baseline as f64) * 100.0
    } else {
        0.0
    };

    TokenSavingsReport {
        baseline_tokens: baseline,
        aden_tokens,
        savings_percent: savings,
        period: "last_30_days".to_string(),
    }
}

pub fn propose_security_directive(
    guard_name: &str,
    reason: &str,
) -> String {
    format!(
        "[security#{}]\n----\n:forbid_import: {}\n{}\n----\n",
        guard_name, guard_name, reason
    )
}

pub fn propose_agent_directive(
    agent_id: &str,
    directive: &str,
    reason: &str,
) -> String {
    format!(
        "[agent#{}]\n----\n{}\nReason: {}\n----\n",
        agent_id, directive, reason
    )
}

fn rfc3339_now() -> String {
    use std::time::UNIX_EPOCH;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();
    format!("{}.{:09}Z", secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_savings_calculation() {
        let report = compute_token_savings(1000, 250);
        assert_eq!(report.savings_percent, 75.0);
        assert_eq!(report.baseline_tokens, 1000);
        assert_eq!(report.aden_tokens, 250);
    }

    #[test]
    fn test_token_savings_zero_baseline() {
        let report = compute_token_savings(0, 0);
        assert_eq!(report.savings_percent, 0.0);
    }

    #[test]
    fn test_guard_telemetry_fired() {
        let event = emit_guard_telemetry("forbid_unsafe", true);
        assert!(matches!(event.event_type, EventType::GuardFired));
        assert_eq!(event.guard_name, Some("forbid_unsafe".to_string()));
    }

    #[test]
    fn test_guard_telemetry_blocked() {
        let event = emit_guard_telemetry("forbid_unsafe", false);
        assert!(matches!(event.event_type, EventType::GuardBlocked));
    }

    #[test]
    fn test_latency_spike() {
        let event = emit_latency_spike("my-contract", 5000);
        assert!(matches!(event.event_type, EventType::LatencySpike));
        assert_eq!(event.latency_ms, Some(5000));
    }
}