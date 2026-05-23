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
use pyo3::prelude::*;
use std::path::Path;

/// Parse a source file and emit each extracted Document as an AsciiDoc string.
#[pyfunction]
fn generate(path: &str) -> PyResult<Vec<String>> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
    let docs = aden_parse::parse_file(Path::new(path), &source)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    let emitted = docs.iter().map(aden_emit::emit_document).collect();
    Ok(emitted)
}

/// Placeholder assembly from an anchor with a token budget.
#[pyfunction]
fn assemble(from_anchor: &str, budget: usize) -> PyResult<String> {
    let out = format!(
        "[[{anchor}]]\n= Assembly from {anchor}\n\nPlaceholder assembly with budget {budget} tokens.\n",
        anchor = from_anchor,
        budget = budget
    );
    Ok(out)
}

/// Check a file for unresolved references and return a report string.
#[pyfunction]
fn check(path: &str) -> PyResult<String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
    match aden_emit::check::verify(&content) {
        Ok(()) => Ok("OK: no unresolved references found.".to_string()),
        Err(report) => Ok(report),
    }
}

/// Python module `aden`.
#[pymodule]
fn aden(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(generate, m)?)?;
    m.add_function(wrap_pyfunction!(assemble, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    Ok(())
}
