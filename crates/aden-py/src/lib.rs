use pyo3::prelude::*;
use std::path::Path;

#[pyfunction]
fn generate(path: &str) -> PyResult<Vec<String>> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
    let docs = aden_parse::parse_file(Path::new(path), &source)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    let emitted = docs.iter().map(aden_emit::emit_document).collect();
    Ok(emitted)
}

#[pyfunction]
fn check(path: &str) -> PyResult<String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
    match aden_emit::check::verify(&content) {
        Ok(()) => Ok("OK: no unresolved references found.".to_string()),
        Err(report) => Ok(report),
    }
}

#[pyfunction]
fn search(project_dir: &str, query: &str, limit: usize) -> PyResult<Vec<PySearchResult>> {
    let path = Path::new(project_dir);
    if !path.is_dir() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "project_dir must be a directory"
        ));
    }

    let index = aden_index::Index::from_directory(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

    let results = index.query(query);
    let limited: Vec<_> = results.iter().take(limit).map(|r| {
        PySearchResult {
            anchor: r.anchor.clone(),
            source_path: r.source_path.to_string_lossy().to_string(),
            snippet: r.snippet.clone(),
            score: r.score,
        }
    }).collect();

    Ok(limited)
}

#[pyfunction]
fn assemble(
    project_dir: &str,
    from_anchor: &str,
    budget: usize,
    depth: usize,
    auto: bool,
) -> PyResult<String> {
    let path = Path::new(project_dir);
    if !path.is_dir() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "project_dir must be a directory"
        ));
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

    let start_anchor = if auto {
        let index = aden_index::Index::from_directory(path)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
        let results = index.query(from_anchor);
        if results.is_empty() {
            from_anchor.to_string()
        } else {
            results.first().map(|r| r.anchor.clone()).unwrap_or_else(|| from_anchor.to_string())
        }
    } else {
        from_anchor.to_string()
    };

    let asm_opts = aden_asm::traverse::AssemblyOptions {
        start_anchor: start_anchor.clone(),
        max_depth: depth,
        token_budget: budget,
        edge_types: Vec::new(),
        block_filter: Vec::new(),
        include_tags: Vec::new(),
        exclude_tags: Vec::new(),
        attributes: Vec::new(),
        llm_mode: false,
    };

    let output = aden_asm::traverse::assemble(&graph, &asm_opts)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

    Ok(output)
}

#[pyfunction]
fn list_anchors(project_dir: &str, limit: usize) -> PyResult<Vec<PyAnchor>> {
    let path = Path::new(project_dir);
    if !path.is_dir() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "project_dir must be a directory"
        ));
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

    let mut anchors = Vec::new();
    for node_idx in graph.graph.node_indices() {
        if anchors.len() >= limit {
            break;
        }
        let node = &graph.graph[node_idx];
        anchors.push(PyAnchor {
            anchor: node.anchor.clone(),
            node_type: format!("{:?}", node.doc.node_type),
        });
    }

    Ok(anchors)
}

#[pyfunction]
fn query(
    project_dir: &str,
    from_anchor: &str,
    depth: usize,
) -> PyResult<String> {
    let path = Path::new(project_dir);
    if !path.is_dir() {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "project_dir must be a directory"
        ));
    }

    let graph = aden_graph::cache::build_from_directory_cached(path)
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

    let start_idx = graph.get_index(from_anchor)
        .ok_or_else(|| pyo3::exceptions::PyValueError::new_err(
            format!("Anchor '{}' not found", from_anchor)
        ))?;

    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back((start_idx, 0usize));

    while let Some((node_idx, d)) = queue.pop_front() {
        if visited.contains(&node_idx) || d > depth {
            continue;
        }
        visited.insert(node_idx);

        let node = &graph.graph[node_idx];
        let title = node.doc.attributes.get("title")
            .cloned()
            .unwrap_or_else(|| node.anchor.clone());
        
        results.push(serde_json::json!({
            "anchor": node.anchor,
            "depth": d,
            "title": title,
        }));

        for neighbor in graph.graph.neighbors_directed(node_idx, petgraph::Direction::Outgoing) {
            if !visited.contains(&neighbor) {
                queue.push_back((neighbor, d + 1));
            }
        }
    }

    Ok(serde_json::to_string_pretty(&results).unwrap_or_default())
}

#[pyclass]
struct PySearchResult {
    #[pyo3(get)]
    anchor: String,
    #[pyo3(get)]
    source_path: String,
    #[pyo3(get)]
    snippet: String,
    #[pyo3(get)]
    score: f64,
}

#[pyclass]
struct PyAnchor {
    #[pyo3(get)]
    anchor: String,
    #[pyo3(get)]
    node_type: String,
}

#[pymodule]
fn aden(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(generate, m)?)?;
    m.add_function(wrap_pyfunction!(assemble, m)?)?;
    m.add_function(wrap_pyfunction!(check, m)?)?;
    m.add_function(wrap_pyfunction!(search, m)?)?;
    m.add_function(wrap_pyfunction!(list_anchors, m)?)?;
    m.add_function(wrap_pyfunction!(query, m)?)?;
    Ok(())
}