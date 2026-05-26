#![allow(clippy::module_inception)]
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
#[cfg(test)]
mod tests {
    use crate::{preprocess, traverse};
    use std::collections::HashMap;

    #[test]
    fn test_preprocess_simple_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            ":key: value\n\n[[anchor]]\n= Title\n\nHello world",
        )
        .unwrap();
        let mut visited = Vec::new();
        let out = preprocess::preprocess(tmp.path(), &HashMap::new(), &mut visited, 0).unwrap();
        assert!(out.contains("[[anchor]]"));
        assert!(out.contains("Hello world"));
    }

    #[test]
    fn test_preprocess_does_not_panic_on_no_includes() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "[[solo]]\n= Solo\n\nNo includes here.").unwrap();
        let mut visited = Vec::new();
        let out = preprocess::preprocess(tmp.path(), &HashMap::new(), &mut visited, 0).unwrap();
        assert!(out.contains("No includes here."));
    }

    #[test]
    fn test_assemble_options_default() {
        let opts = traverse::AssemblyOptions {
            start_anchor: "start".to_string(),
            max_depth: 3,
            token_budget: 1000,
            edge_types: vec![],
            block_filter: vec![],
            include_tags: vec![],
            exclude_tags: vec![],
            attributes: vec![],
        };
        assert_eq!(opts.start_anchor, "start");
        assert_eq!(opts.max_depth, 3);
    }
}
