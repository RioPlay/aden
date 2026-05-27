// LLM-optimized context assembly output
// This module formats aden context in ways LLMs find most useful

use aden_graph::AdenGraph;

/// Format context assembly for LLM consumption
pub fn format_for_llm(
    graph: &AdenGraph,
    assembled: &str,
    query: &str,
    anchor: &str,
    depth: usize,
    budget: usize,
) -> String {
    let mut output = String::new();
    
    // Header with metadata LLMs need
    output.push_str("<!-- ADEN CONTEXT ASSEMBLY -->\n");
    output.push_str(&format!("<!-- Query: {} -->\n", query));
    output.push_str(&format!("<!-- Source Anchor: {} -->\n", anchor));
    output.push_str(&format!("<!-- Traversal Depth: {} -->\n", depth));
    output.push_str(&format!("<!-- Token Budget: {} -->\n", budget));
    output.push_str(&format!("<!-- Assembled Size: {} chars -->\n", assembled.len()));
    output.push_str("\n");
    
    // Extract and list sources
    output.push_str("## Sources\n\n");
    for line in assembled.lines() {
        if line.starts_with(":source_file:") {
            let source = line.strip_prefix(":source_file: ").unwrap_or(line);
            output.push_str(&format!("- `{}`\n", source));
        }
    }
    output.push_str("\n");
    
    // Full context
    output.push_str("## Context\n\n");
    output.push_str(assembled);
    
    output
}

/// Convert AsciiDoc to Markdown for LLM compatibility
pub fn adoc_to_md(adoc: &str) -> String {
    let mut md = adoc.to_string();
    
    // Convert AsciiDoc headers to Markdown
    // This is a simple conversion - full implementation would be more robust
    for i in 1..=6 {
        let pattern = format!("{} ", "=".repeat(i));
        let replacement = format!("{} ", "#".repeat(i));
        md = md.replace(&pattern, &replacement);
    }
    
    // Convert AsciiDoc links to Markdown
    // <<anchor,text>> -> [text](#anchor)
    // This is simplified - real implementation needs proper parsing
    
    // Convert AsciiDoc code blocks
    md = md.replace("----", "```");
    md = md.replace("====", "```");
    
    md
}
