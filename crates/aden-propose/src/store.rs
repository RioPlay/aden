// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// All rights reserved.
//
// Aden: A Dense Referential Context Compiler
// Original author and maintainer: RioPlay
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
use std::fs::{self, File};
use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use crate::{Proposal, ProposalStatus};

fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

pub fn persist(proposal: &Proposal, store_dir: &Path) -> io::Result<PathBuf> {
    if !is_safe_id(&proposal.id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid proposal id"));
    }
    let proposals_dir = store_dir.join(".aden").join("proposals");
    fs::create_dir_all(&proposals_dir)?;
    let path = proposals_dir.join(format!("{}.patch.adoc", proposal.id));
    let mut file = File::create(&path)?;
    file.write_all(proposal.patch_asciidoc.as_bytes())?;
    Ok(path)
}

pub fn load(id: &str, store_dir: &Path) -> io::Result<Proposal> {
    if !is_safe_id(id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid proposal id"));
    }
    let proposals_dir = store_dir.join(".aden").join("proposals");
    let path = proposals_dir.join(format!("{}.patch.adoc", id));
    let mut file = File::open(&path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    parse_proposal(&contents, &path)
}

pub fn list(store_dir: &Path) -> io::Result<Vec<Proposal>> {
    let proposals_dir = store_dir.join(".aden").join("proposals");
    let mut proposals = Vec::new();
    if !proposals_dir.exists() {
        return Ok(proposals);
    }
    for entry in fs::read_dir(&proposals_dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Some(ext) = path.extension()
            && ext == "adoc" {
                let mut file = File::open(&path)?;
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;
                if let Ok(proposal) = parse_proposal(&contents, &path) {
                    proposals.push(proposal);
                }
            }
    }
    Ok(proposals)
}

pub fn apply(proposal: &Proposal) -> io::Result<()> {
    if !is_safe_id(&proposal.id) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid proposal id"));
    }

    // For MissingContract, create the new contract file
    match proposal.drift_type.as_str() {
        "MissingContract" => {
            if let Some(parent) = proposal.target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&proposal.target_path, &proposal.patch_asciidoc)?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Drift type '{}' requires manual review before application",
                    proposal.drift_type
                ),
            ));
        }
    }

    Ok(())
}

fn parse_proposal(contents: &str, path: &Path) -> io::Result<Proposal> {
    let id = contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with("[[") && line.ends_with("]]") {
                Some(
                    line.trim_start_matches("[[")
                        .trim_end_matches("]]")
                        .to_string(),
                )
            } else {
                None
            }
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing proposal id"))?;

    let status = contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with(":status:") {
                let val = line.trim_start_matches(":status:").trim();
                val.parse::<ProposalStatus>().ok()
            } else {
                None
            }
        })
        .unwrap_or(ProposalStatus::PendingReview);

    let confidence = contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with(":confidence:") {
                line.trim_start_matches(":confidence:")
                    .trim()
                    .parse::<f64>()
                    .ok()
            } else {
                None
            }
        })
        .unwrap_or(0.5);

    let target_path = contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with(":target:") {
                Some(PathBuf::from(line.trim_start_matches(":target:").trim()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| path.to_path_buf());

    let drift_type = contents
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with(":drift_type:") {
                Some(line.trim_start_matches(":drift_type:").trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default();

    let rationale = if let Some(start) = contents.find("== Rationale") {
        let start_idx = start + "== Rationale".len();
        let substr = &contents[start_idx..];
        let end = substr.find("\n== ").unwrap_or(substr.len());
        substr[..end].trim().to_string()
    } else {
        String::new()
    };

    Ok(Proposal {
        id,
        target_path,
        drift_type,
        confidence,
        status,
        rationale,
        patch_asciidoc: contents.to_string(),
    })
}
