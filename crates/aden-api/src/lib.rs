// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// SPDX-License-Identifier: AGPL-3.0-or-later
//!
//! API module for Aden: REST/gRPC for contract queries, overrides, SBOM export.
//!
//! Phase 6 of the Aden roadmap.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractStateRequest {
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideProposal {
    pub directive_tag: String,
    pub justification: String,
    pub reviewer: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SbomExportRequest {
    pub format: SbomFormat,
    pub include_dependencies: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SbomFormat {
    SPDX,
    CycloneDX,
    SLSA,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FederationConfig {
    pub registry_url: String,
    pub api_key: Option<String>,
    pub repositories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub directives: Vec<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRepoEdge {
    pub source_repo: String,
    pub source_anchor: String,
    pub target_repo: String,
    pub target_anchor: String,
    pub edge_type: String,
}

pub fn query_contract_state(anchor: &str) -> ApiResponse<HashMap<String, String>> {
    ApiResponse {
        success: true,
        data: Some(HashMap::from([
            ("anchor".to_string(), anchor.to_string()),
            ("status".to_string(), "active".to_string()),
        ])),
        error: None,
    }
}

pub fn submit_override(proposal: OverrideProposal) -> ApiResponse<String> {
    if proposal.justification.is_empty() {
        return ApiResponse {
            success: false,
            data: None,
            error: Some("Justification required".to_string()),
        };
    }
    if proposal.reviewer.is_empty() {
        return ApiResponse {
            success: false,
            data: None,
            error: Some("Reviewer required".to_string()),
        };
    }

    ApiResponse {
        success: true,
        data: Some(format!(
            "Override proposal {} submitted for review",
            proposal.directive_tag
        )),
        error: None,
    }
}

pub fn fetch_policy_template(
    _config: &FederationConfig,
    template_id: &str,
) -> ApiResponse<PolicyTemplate> {
    ApiResponse {
        success: true,
        data: Some(PolicyTemplate {
            id: template_id.to_string(),
            name: "default".to_string(),
            description: "Default policy template".to_string(),
            directives: vec![":forbid_import: unsafe::*".to_string()],
            version: "1.0".to_string(),
        }),
        error: None,
    }
}

pub fn resolve_cross_repo_edges(edges: &[CrossRepoEdge]) -> Vec<CrossRepoEdge> {
    edges.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_contract_state() {
        let response = query_contract_state("test-anchor");
        assert!(response.success);
        assert!(response.data.is_some());
    }

    #[test]
    fn test_submit_override_valid() {
        let proposal = OverrideProposal {
            directive_tag: "forbid_unsafe".to_string(),
            justification: "Need for FFI".to_string(),
            reviewer: "alice".to_string(),
            expires_at: None,
        };
        let response = submit_override(proposal);
        assert!(response.success);
    }

    #[test]
    fn test_submit_override_missing_justification() {
        let proposal = OverrideProposal {
            directive_tag: "forbid_unsafe".to_string(),
            justification: "".to_string(),
            reviewer: "alice".to_string(),
            expires_at: None,
        };
        let response = submit_override(proposal);
        assert!(!response.success);
        assert!(response.error.is_some());
    }

    #[test]
    fn test_submit_override_missing_reviewer() {
        let proposal = OverrideProposal {
            directive_tag: "forbid_unsafe".to_string(),
            justification: "Need for FFI".to_string(),
            reviewer: "".to_string(),
            expires_at: None,
        };
        let response = submit_override(proposal);
        assert!(!response.success);
    }

    #[test]
    fn test_fetch_policy_template() {
        let config = FederationConfig {
            registry_url: "https:// aden.dev".to_string(),
            api_key: None,
            repositories: vec![],
        };
        let response = fetch_policy_template(&config, "template-1");
        assert!(response.success);
        assert!(response.data.is_some());
    }

    #[test]
    fn test_cross_repo_edges() {
        let edges = vec![CrossRepoEdge {
            source_repo: "repo-a".to_string(),
            source_anchor: "anchor1".to_string(),
            target_repo: "repo-b".to_string(),
            target_anchor: "anchor2".to_string(),
            edge_type: "uses".to_string(),
        }];
        let resolved = resolve_cross_repo_edges(&edges);
        assert_eq!(resolved.len(), 1);
    }
}
