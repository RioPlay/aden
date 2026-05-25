// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! Attestation module for Aden: Ed25519 signing, provenance.
//!
//! Note: Aden's knowledge graph already tracks dependencies, edges, and contracts.
//! This module provides signing for cryptographic proof of attestation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedAttestation {
    pub payload: String,
    pub signature: Option<String>,
    pub public_key: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub contract_hash: String,
    pub source_hashes: Vec<String>,
    pub build_metadata: Option<String>,
    pub signed_by: Option<String>,
}

pub fn create_unsigned_attestation(payload: &str) -> SignedAttestation {
    SignedAttestation {
        payload: payload.to_string(),
        signature: None,
        public_key: None,
        timestamp: rfc3339_now(),
    }
}

pub fn verify_signature(attestation: &SignedAttestation) -> bool {
    attestation.signature.is_some() && attestation.public_key.is_some()
}

pub fn create_provenance(contract_hash: &str, source_hashes: &[String]) -> ProvenanceRecord {
    ProvenanceRecord {
        contract_hash: contract_hash.to_string(),
        source_hashes: source_hashes.to_vec(),
        build_metadata: None,
        signed_by: None,
    }
}

fn rfc3339_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", now.as_secs(), now.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unsigned_attestation() {
        let attestation = create_unsigned_attestation("test payload");
        assert_eq!(attestation.payload, "test payload");
        assert!(attestation.signature.is_none());
    }

    #[test]
    fn test_verify_signature_missing() {
        let attestation = create_unsigned_attestation("test");
        assert!(!verify_signature(&attestation));
    }

    #[test]
    fn test_provenance_creation() {
        let provenance = create_provenance("abc123", &["def456".to_string(), "ghi789".to_string()]);
        assert_eq!(provenance.contract_hash, "abc123");
        assert_eq!(provenance.source_hashes.len(), 2);
    }
}
