// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Protocol-neutral Context Receipt schema.
//!
//! Receipts are metadata about an answer, kept under a dedicated
//! `context_receipt` member rather than flattened into result data. Receipt v1
//! deliberately carries only its schema version and Aden's existing freshness
//! label. AP-102 through AP-106 own all later metadata semantics.

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CONTEXT_RECEIPT_SCHEMA_VERSION: u16 = 1;

/// Machine-readable metadata accompanying an Aden result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextReceipt {
    /// Independently versioned receipt schema.
    pub schema_version: u16,
    /// Existing freshness wire label, mirrored without changing its semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<ReceiptFreshness>,
}

impl ContextReceipt {
    pub fn new() -> Self {
        Self {
            schema_version: CONTEXT_RECEIPT_SCHEMA_VERSION,
            freshness: None,
        }
    }

    /// Reject receipt versions this build cannot interpret.
    pub fn validate_supported(&self) -> Result<(), ReceiptSchemaError> {
        if self.schema_version == CONTEXT_RECEIPT_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(ReceiptSchemaError::UnsupportedVersion(self.schema_version))
        }
    }

    pub fn with_freshness(mut self, freshness: ReceiptFreshness) -> Self {
        self.freshness = Some(freshness);
        self
    }
}

impl Default for ContextReceipt {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReceiptSchemaError {
    #[error("unsupported Context Receipt schema version: {0}")]
    UnsupportedVersion(u16),
}

/// Compatibility labels mirroring Aden's existing freshness wire values.
/// AP-103 owns any future migration from these labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptFreshness {
    Current,
    Snapshot,
    Lagging,
    Building,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_a_minimal_versioned_receipt() {
        assert_eq!(
            serde_json::to_value(ContextReceipt::new()).unwrap(),
            serde_json::json!({"schema_version": 1})
        );
    }

    #[test]
    fn receipt_keeps_metadata_in_its_own_namespace() {
        let receipt = ContextReceipt::new().with_freshness(ReceiptFreshness::Current);
        let envelope =
            serde_json::json!({"result":{"freshness":"payload"}, "context_receipt":receipt});
        assert_eq!(envelope["result"]["freshness"], "payload");
        assert_eq!(envelope["context_receipt"]["freshness"], "current");
    }

    #[test]
    fn unknown_additive_fields_are_accepted_but_unknown_versions_are_not() {
        let v1: ContextReceipt =
            serde_json::from_value(serde_json::json!({"schema_version":1,"future":true})).unwrap();
        v1.validate_supported().unwrap();
        let v2: ContextReceipt =
            serde_json::from_value(serde_json::json!({"schema_version":2})).unwrap();
        assert_eq!(
            v2.validate_supported(),
            Err(ReceiptSchemaError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn default_and_new_always_emit_v1() {
        for receipt in [ContextReceipt::default(), ContextReceipt::new()] {
            assert_eq!(serde_json::to_value(receipt).unwrap()["schema_version"], 1);
        }
    }
}
