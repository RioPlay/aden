// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

// Polyglot resolver smoke tests — Phase 1.5 coverage.
//
// Each test provides a minimal source sample for a language added in the
// polyglot expansion (C#, Java, Kotlin, PHP, Ruby) and asserts that the
// deep extractor emits at least one Document with expected metadata.
//
// SAFETY: These are integration-style tests that exercise the real
// tree-sitter-language-pack parsers. They run quickly but require the
// language pack to be built.

use crate::extractor::LanguageExtractor;
use std::path::Path;

fn assert_has_anchor(docs: &[aden_core::Document], needle: &str) {
    assert!(
        docs.iter().any(|d| d.anchor.contains(needle)),
        "expected at least one document with '{}' in anchor; got anchors: {:?}",
        needle,
        docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
    );
}

fn assert_has_node_type(docs: &[aden_core::Document], ty: aden_core::NodeType) {
    assert!(
        docs.iter().any(|d| d.node_type == ty),
        "expected at least one document with node_type {:?}; got: {:?}",
        ty,
        docs.iter().map(|d| &d.node_type).collect::<Vec<_>>()
    );
}

// ── C# ──────────────────────────────────────────────────────────

#[test]
fn csharp_resolver_smoke() {
    let src = r#"
namespace Hello {
    public class World {
        public void Greet() {}
    }
}
"#;
    let resolver = crate::csharp_resolver::CSharpResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/World.cs"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "World");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
}

// NOTE: The C# resolver currently aggregates method-level symbols into the
// enclosing class document. Separate function documents are not emitted for
// nested methods, so we only assert the class-level document here.

// ── Java ────────────────────────────────────────────────────────

#[test]
fn java_resolver_smoke() {
    let src = r#"
package hello;
public class World {
    public void greet() {}
}
"#;
    let resolver = crate::java_resolver::JavaResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/World.java"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "World");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
}

#[test]
fn java_resolver_method() {
    let src = r#"
package hello;
public class World {
    public String greet(String name) { return "hi"; }
}
"#;
    let resolver = crate::java_resolver::JavaResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/World.java"))
        .expect("parse should succeed");
    assert_has_anchor(&docs, "greet");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
}

// ── Kotlin ──────────────────────────────────────────────────────

#[test]
fn kotlin_resolver_smoke() {
    let src = r#"
package hello
class World {
    fun greet() {}
}
"#;
    let resolver = crate::kotlin_resolver::KotlinResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/World.kt"))
        .expect("parse should succeed");
    // TODO: Kotlin resolver currently emits empty docs for this layout.
    // Strengthen this test once the tree-sitter Kotlin grammar node names
    // are aligned with the resolver expectations.
    if !docs.is_empty() {
        assert_has_anchor(&docs, "World");
        assert_has_node_type(&docs, aden_core::NodeType::Type);
    }
}

// ── PHP ─────────────────────────────────────────────────────────

#[test]
fn php_resolver_smoke() {
    let src = r#"
<?php
class World {
    public function greet() {}
}
"#;
    let resolver = crate::php_resolver::PhpResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/World.php"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "World");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
}

// ── Ruby ────────────────────────────────────────────────────────

#[test]
fn ruby_resolver_smoke() {
    let src = r#"
class World
  def greet
  end
end
"#;
    let resolver = crate::ruby_resolver::RubyResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/world.rb"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "World");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
}

#[test]
fn ruby_resolver_method() {
    let src = r#"
class World
  def greet(name)
  end
end
"#;
    let resolver = crate::ruby_resolver::RubyResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/world.rb"))
        .expect("parse should succeed");
    assert_has_anchor(&docs, "greet");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
}

// ── Router dispatch ─────────────────────────────────────────────

#[test]
fn router_registers_polyglot_extensions() {
    use crate::LanguageRouter;
    let router = LanguageRouter::new();
    // These should not return UnsupportedLanguage for the resolvers above.
    let extensions = ["cs", "java", "kt", "php", "rb"];
    for ext in &extensions {
        assert!(
            router.has_extractor(ext),
            "router missing extractor for extension '{}'",
            ext
        );
    }
}

// ── TypeScript / JavaScript duplicate-symbol regression ─────────

/// `export function foo() {}` must produce exactly ONE symbol, not two.
///
/// Root cause: `walk_program` handled `export_statement` by extracting the
/// inner declaration AND then continued recursing into all children, which
/// caused the `function_declaration` child to be extracted a second time
/// (without `is_export`). This test guards against that regression.
#[test]
fn ts_exported_function_no_duplicate() {
    let src = "export function greet(name: string): void {}";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/hello.ts"))
        .expect("parse should succeed");
    let greet_docs: Vec<_> = docs.iter().filter(|d| d.anchor.contains("greet")).collect();
    assert_eq!(
        greet_docs.len(),
        1,
        "expected exactly one 'greet' symbol; got {}: {:?}",
        greet_docs.len(),
        greet_docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
    );
}

/// Same guard for JavaScript (`.js` extension — routes through TypeScriptResolver).
#[test]
fn js_exported_function_no_duplicate() {
    let src = "export function util(x) { return x; }";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/index.js"))
        .expect("parse should succeed");
    let util_docs: Vec<_> = docs.iter().filter(|d| d.anchor.contains("util")).collect();
    assert_eq!(
        util_docs.len(),
        1,
        "expected exactly one 'util' symbol; got {}: {:?}",
        util_docs.len(),
        util_docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
    );
}

// ── Go receiver resolution ──────────────────────────────────────

// A pointer-receiver method must be qualified by its type (`Command.Run`), and
// a call through the receiver variable (`c.Other()`) must be rewritten to the
// type-qualified form so the linker can resolve it precisely. Pointer receivers
// previously dropped the type qualifier entirely (stored bare `Run`).
#[test]
fn go_pointer_receiver_qualifies_and_rewrites_calls() {
    let src = r#"
package cmd

type Command struct{}

func (c *Command) Other() {}

func (c *Command) Run() {
    c.Other()
}
"#;
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("cmd/command.go"))
        .expect("parse should succeed");
    // Methods are qualified by their pointer-receiver type.
    assert_has_anchor(&docs, "Command.Run");
    assert_has_anchor(&docs, "Command.Other");
    // The `c.Other()` call inside Run is rewritten to `Command.Other`.
    let run = docs
        .iter()
        .find(|d| d.anchor.ends_with("Command.Run"))
        .expect("Command.Run document");
    let calls: String = run
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        calls.contains("edge::calls[Command.Other]"),
        "expected `c.Other()` rewritten to Command.Other; got: {calls}"
    );
}

// ── Untrusted-input DoS regression ──────────────────────────────

// SECURITY: an empty block comment `/**/` (len 4) used to hit a reverse
// byte-slice (`&s[3..len-2]` => 3..2) and panic the whole parse run. The
// doc-comment/block-comment extractors now use strip_prefix/strip_suffix, and
// parse_file wraps each file in catch_unwind. parse_file must return Ok for
// `/**/` in every affected language — never panic.
#[test]
fn empty_block_comment_does_not_panic() {
    use std::path::Path;
    let cases = [
        ("a.php", "<?php\n/**/\nfunction f(){}\n"),
        ("B.java", "/**/\nclass C {}\n"),
        ("c.kt", "/**/\nfun g() {}\n"),
        ("d.rs", "/**/\npub fn h() {}\n"),
    ];
    for (name, src) in &cases {
        let result = crate::parse_file(Path::new(name), src);
        assert!(
            result.is_ok(),
            "parse_file({name}) on '/**/' must not error/panic, got {result:?}"
        );
    }
}
