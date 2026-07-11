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
//
// Root cause of the previous guarded `if !docs.is_empty()`: the tree-sitter
// Kotlin grammar does NOT expose function names through a `name` field — the
// name is a bare `simple_identifier` child. `parse_function` now falls back
// to the first `simple_identifier` child, so both class and method documents
// are emitted unconditionally.

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
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "World");
    assert_has_anchor(&docs, "greet");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
    assert_has_node_type(&docs, aden_core::NodeType::Function);
}

// ── Python ──────────────────────────────────────────────────────
//
// The Python resolver is a 700-line tree-sitter extractor. These tests guard
// three specific behaviors:
//   1. Docstring extraction — `extract_preceding_docstring` reads the first
//      string child of a function body (fixed: tree-sitter-python emits the
//      docstring as `block > string` or `block > expression_statement > string`
//      depending on grammar version).
//   2. Dot-qualified anchors for class methods (`Class.method`).
//   3. `edge::calls[callee]` Listing blocks for intra-module call sites.

/// A top-level function with a triple-quoted docstring must produce a Function
/// document whose anchor contains `compute_checksum` and whose blocks include
/// a Paragraph with the docstring text.
#[test]
fn python_resolver_smoke() {
    let src = "def compute_checksum(data):\n    \"\"\"Compute checksum.\"\"\"\n    pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/utils.py"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "compute_checksum");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
    // The docstring body must appear in a Paragraph block (guards the
    // extract_preceding_docstring fix for `block > string` emission).
    let func_doc = docs
        .iter()
        .find(|d| d.anchor.contains("compute_checksum"))
        .expect("compute_checksum document");
    let has_docstring = func_doc.blocks.iter().any(|b| match b {
        aden_core::Block::Paragraph(text) => text.contains("Compute checksum"),
        _ => false,
    });
    assert!(
        has_docstring,
        "expected docstring text in a Paragraph block; blocks: {:?}",
        func_doc.blocks
    );
}

/// A class method must produce a dot-qualified anchor (`MyClass.my_method`),
/// not a bare `my_method`, so two same-named methods in different classes
/// cannot collapse to the same anchor.
#[test]
fn python_resolver_class_method() {
    let src = "class MyClass:\n    def my_method(self):\n        pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/myclass.py"))
        .expect("parse should succeed");
    // Class document
    assert_has_anchor(&docs, "MyClass");
    assert_has_node_type(&docs, aden_core::NodeType::Type);
    // Method document — anchor must be dot-qualified
    assert_has_anchor(&docs, "MyClass.my_method");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
}

/// When function A calls function B in its body, A's document must include a
/// Listing block containing `edge::calls[function_b]`. This guards the
/// call-site resolution and edge emission pipeline.
#[test]
fn python_resolver_call_sites() {
    let src = "def function_a(x):\n    return function_b(x)\n\ndef function_b(x):\n    return x\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/mod.py"))
        .expect("parse should succeed");
    assert_has_anchor(&docs, "function_a");
    assert_has_anchor(&docs, "function_b");
    let func_a = docs
        .iter()
        .find(|d| d.anchor.contains("function_a"))
        .expect("function_a document");
    let calls_text: String = func_a
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        calls_text.contains("edge::calls[function_b]"),
        "expected `edge::calls[function_b]` in function_a's Listing blocks; got: {calls_text}"
    );
}

#[test]
fn python_self_method_call_emits_self_prefix() {
    // `self.prepare()` must emit `self.prepare` so the linker re-qualifies it to
    // `Command.prepare` (the zero-FP self path). End-to-end this is what gives
    // method callers a blast radius on OO Python.
    let src = "class Command:\n\
               \x20   def invoke(self):\n\
               \x20       self.prepare()\n\
               \x20   def prepare(self):\n\
               \x20       pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/cmd.py"))
        .expect("parse should succeed");
    let invoke = docs
        .iter()
        .find(|d| d.anchor.contains("invoke"))
        .expect("invoke document");
    let calls_text: String = invoke
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        calls_text.contains("edge::calls[self.prepare]"),
        "self-method call must emit `self.prepare`; got: {calls_text}"
    );
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

// ── PowerShell ──────────────────────────────────────────────────
//
// PowerShell is parsed in-process via the airbus-cert tree-sitter grammar
// through the GenericExtractor — no external `pwsh`/`Export-AST.ps1` bridge.
// This guards the PowerShell-specific node kinds taught to the generic
// walker (`function_statement`, `class_statement`, `class_method_definition`,
// and the `function_name`/`simple_name` name nodes).

// PowerShell is not in the build-time grammar set (TSLP_LANGUAGES), so it loads
// only when `grammars-download` can fetch it (or a prior download seeded the
// cache). Gate the test behind that feature so the default, network-free
// `cargo test` stays deterministic on a fresh CI.
#[cfg(all(feature = "generic", feature = "grammars-download"))]
#[test]
fn powershell_generic_smoke() {
    let src = r#"
function Get-Greeting {
    param([string]$Name)
    Write-Output "Hello, $Name"
}

class Widget {
    [string]$Label
    [string] Render() { return $this.Label }
}
"#;
    let extractor = crate::generic::GenericExtractor::new("powershell");
    let docs = extractor
        .extract_documents(src, Path::new("src/sample.psm1"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "Get-Greeting");
    assert_has_anchor(&docs, "Widget");
    assert_has_anchor(&docs, "Render");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
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
    let extensions = ["cs", "java", "kt", "php", "rb", "rst"];
    for ext in &extensions {
        assert!(
            router.has_extractor(ext),
            "router missing extractor for extension '{}'",
            ext
        );
    }
}

#[test]
fn router_extracts_rst_as_source_spanned_prose_paragraphs() {
    use crate::LanguageRouter;
    let docs = LanguageRouter::new()
        .parse_file(
            Path::new("docs/factory.rst"),
            "Application Factory\n===================\n\nCreate the app here.\n\nRegister the blueprint next.\n",
        )
        .expect("RST fallback should parse");
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().all(is_locatable));
    assert!(
        docs.iter()
            .all(|doc| doc.node_type == aden_core::NodeType::Note)
    );
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

// ── Wave 1 graph types: implements emission — Java, TypeScript, Python ──

/// Helper: collect all Listing-block text from the doc whose anchor contains
/// `needle`. Returns the joined code strings, or panics with the full anchor
/// list when no matching document exists.
fn listing_text_by_anchor(docs: &[aden_core::Document], needle: &str) -> String {
    let doc = docs
        .iter()
        .find(|d| d.anchor.contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "no document with '{}' in anchor; got: {:?}",
                needle,
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    doc.blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Java ────────────────────────────────────────────────────────────────────

const JAVA_IMPLEMENTS_FIXTURE: &str = r#"
package com.example;

public class Foo implements Bar, Baz {
    public void doThing() {}
}
"#;

const JAVA_EXTENDS_FIXTURE: &str = r#"
package com.example;

public class Child extends Parent {
    public void act() {}
}
"#;

/// `class Foo implements Bar, Baz` must emit `edge::implements[Bar]` and
/// `edge::implements[Baz]` on the Foo class document.
#[test]
fn java_implements_emits_edge_macros() {
    let resolver = crate::java_resolver::JavaResolver::new();
    let docs = resolver
        .extract_documents(JAVA_IMPLEMENTS_FIXTURE, std::path::Path::new("Foo.java"))
        .expect("parse should succeed");
    let listing = listing_text_by_anchor(&docs, "Foo");
    assert!(
        listing.contains("edge::implements[Bar]"),
        "class Foo implements Bar must emit edge::implements[Bar]; got: {listing}"
    );
    assert!(
        listing.contains("edge::implements[Baz]"),
        "class Foo implements Bar, Baz must emit edge::implements[Baz]; got: {listing}"
    );
}

/// `class Child extends Parent` must emit `edge::extends[Parent]` on the
/// Child class document. Inheritance is distinct from interface satisfaction.
#[test]
fn java_extends_emits_edge_macro() {
    let resolver = crate::java_resolver::JavaResolver::new();
    let docs = resolver
        .extract_documents(JAVA_EXTENDS_FIXTURE, std::path::Path::new("Child.java"))
        .expect("parse should succeed");
    let listing = listing_text_by_anchor(&docs, "Child");
    assert!(
        listing.contains("edge::extends[Parent]"),
        "class Child extends Parent must emit edge::extends[Parent]; got: {listing}"
    );
    assert!(
        !listing.contains("edge::implements[Parent]"),
        "superclass must NOT appear as edge::implements; got: {listing}"
    );
}

// ── TypeScript ──────────────────────────────────────────────────────────────

const TS_IMPLEMENTS_FIXTURE: &str = r#"
class Foo implements IBar, IBaz {
  greet(): string { return "hi"; }
}
"#;

const TS_EXTENDS_FIXTURE: &str = r#"
class Child extends Base {
  act(): void {}
}
"#;

const TS_EXTENDS_AND_IMPLEMENTS_FIXTURE: &str = r#"
class Derived extends Base implements IFoo {
  work(): void {}
}
"#;

/// `class Foo implements IBar, IBaz` must emit `edge::implements[IBar]` and
/// `edge::implements[IBaz]` on the Foo class document.
#[test]
fn ts_implements_emits_edge_macros() {
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(TS_IMPLEMENTS_FIXTURE, std::path::Path::new("foo.ts"))
        .expect("parse should succeed");
    let listing = listing_text_by_anchor(&docs, "Foo");
    assert!(
        listing.contains("edge::implements[IBar]"),
        "class Foo implements IBar must emit edge::implements[IBar]; got: {listing}"
    );
    assert!(
        listing.contains("edge::implements[IBaz]"),
        "class Foo implements IBar, IBaz must emit edge::implements[IBaz]; got: {listing}"
    );
}

/// `class Child extends Base` must emit `edge::extends[Base]` on the Child
/// class document. Must NOT produce `edge::implements[Base]`.
#[test]
fn ts_extends_emits_edge_macro() {
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(TS_EXTENDS_FIXTURE, std::path::Path::new("child.ts"))
        .expect("parse should succeed");
    let listing = listing_text_by_anchor(&docs, "Child");
    assert!(
        listing.contains("edge::extends[Base]"),
        "class Child extends Base must emit edge::extends[Base]; got: {listing}"
    );
    assert!(
        !listing.contains("edge::implements[Base]"),
        "superclass must NOT appear as edge::implements; got: {listing}"
    );
}

/// `class Derived extends Base implements IFoo` must emit both `edge::extends[Base]`
/// and `edge::implements[IFoo]` — inheritance and interface satisfaction are distinct.
#[test]
fn ts_extends_and_implements_emit_separate_edges() {
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(
            TS_EXTENDS_AND_IMPLEMENTS_FIXTURE,
            std::path::Path::new("derived.ts"),
        )
        .expect("parse should succeed");
    let listing = listing_text_by_anchor(&docs, "Derived");
    assert!(
        listing.contains("edge::extends[Base]"),
        "class Derived extends Base must emit edge::extends[Base]; got: {listing}"
    );
    assert!(
        listing.contains("edge::implements[IFoo]"),
        "class Derived implements IFoo must emit edge::implements[IFoo]; got: {listing}"
    );
}

// ── Python ──────────────────────────────────────────────────────────────────

const PYTHON_BASES_FIXTURE: &str = r#"class Foo(Bar, Baz):
    pass

class SkipObject(object):
    pass

class SkipMeta(Base, metaclass=Meta):
    pass
"#;

/// Plain identifier bases must emit `edge::implements[Base]` for each.
/// `object` is skipped (universal base, no semantic content).
/// `metaclass=Meta` kwargs are skipped (not a superclass).
#[test]
fn python_bases_emit_edge_macros() {
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(PYTHON_BASES_FIXTURE, std::path::Path::new("foo.py"))
        .expect("parse should succeed");

    // Foo(Bar, Baz) → both bases emitted
    let foo_listing = listing_text_by_anchor(&docs, "Foo");
    assert!(
        foo_listing.contains("edge::implements[Bar]"),
        "class Foo(Bar, Baz) must emit edge::implements[Bar]; got: {foo_listing}"
    );
    assert!(
        foo_listing.contains("edge::implements[Baz]"),
        "class Foo(Bar, Baz) must emit edge::implements[Baz]; got: {foo_listing}"
    );

    // SkipObject(object) → no edge emitted (object is the universal Python base)
    let skip_listing = listing_text_by_anchor(&docs, "SkipObject");
    assert!(
        !skip_listing.contains("edge::implements[object]"),
        "class SkipObject(object) must NOT emit edge::implements[object]; got: {skip_listing}"
    );

    // SkipMeta(Base, metaclass=Meta) → only Base emitted, not Meta
    let meta_listing = listing_text_by_anchor(&docs, "SkipMeta");
    assert!(
        meta_listing.contains("edge::implements[Base]"),
        "class SkipMeta(Base, metaclass=Meta) must emit edge::implements[Base]; got: {meta_listing}"
    );
    assert!(
        !meta_listing.contains("edge::implements[Meta]"),
        "metaclass= kwarg must NOT produce edge::implements[Meta]; got: {meta_listing}"
    );
}

// ── Wave 1 graph types: Implements / Mutates emission (Rust) ────
//
// Eval-first tests for the graph-type roadmap Wave 1 (see
// research/topics/aden-roadmap/graph-type-roadmap.adoc). A trait impl's
// methods must carry an `edge::implements[Trait::method]` macro so the
// linker can connect implementor → trait (method-level when the trait
// method anchor exists, trait-level fallback otherwise). A `&mut self`
// receiver must carry `edge::mutates[Type]`.

/// All Listing-block text of the doc whose anchor ends with `suffix`.
fn listing_text(docs: &[aden_core::Document], suffix: &str) -> String {
    let doc = docs
        .iter()
        .find(|d| d.anchor.ends_with(suffix))
        .unwrap_or_else(|| {
            panic!(
                "no document with anchor suffix '{}'; got: {:?}",
                suffix,
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    doc.blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const RUST_TRAIT_FIXTURE: &str = r#"
pub trait Greeter {
    fn greet(&self) -> String;
}

pub struct English;

impl Greeter for English {
    fn greet(&self) -> String {
        make_greeting("hello")
    }
}

impl English {
    pub fn shout(&mut self) {
        make_greeting("HELLO");
    }
    pub fn whisper(&self) {}
}

// A generic-args trait must still link by its base name.
impl From<u8> for English {
    fn from(_v: u8) -> Self {
        English
    }
}

pub fn make_greeting(word: &str) -> String {
    word.to_uppercase()
}
"#;

#[test]
fn rust_trait_impl_emits_implements_macro() {
    let docs = crate::rust::extract_documents_inner(
        std::path::Path::new("src/greeter.rs"),
        RUST_TRAIT_FIXTURE,
    )
    .expect("parse should succeed");
    let greet = listing_text(&docs, "English::greet");
    assert!(
        greet.contains("edge::implements[Greeter::greet]"),
        "trait-impl method must emit a method-qualified implements macro; got: {greet}"
    );
    // Generic args on the trait are stripped so the base name can resolve.
    let from = listing_text(&docs, "English::from");
    assert!(
        from.contains("edge::implements[From::from]"),
        "generic trait `From<u8>` must emit its base name; got: {from}"
    );
}

#[test]
fn rust_self_method_call_emits_qualified_callee() {
    // `self.flush()` must emit `self.flush` (not bare `flush`) so the linker's
    // zero-FP self path re-qualifies it to `Engine::flush`. Bare `flush` would be
    // ambiguous across types and get dropped — the OO blast-radius gap.
    let src = "pub struct Engine;\n\
               impl Engine {\n\
               \x20   pub fn run(&self) { self.flush(); }\n\
               \x20   pub fn flush(&self) {}\n\
               }\n";
    let docs = crate::rust::extract_documents_inner(std::path::Path::new("src/engine.rs"), src)
        .expect("parse should succeed");
    let run = listing_text(&docs, "Engine::run");
    assert!(
        run.contains("edge::calls[self.flush]"),
        "self-method call must emit a self-qualified callee; got: {run}"
    );
}

#[test]
fn rust_self_method_call_with_turbofish_emits_qualified_callee() {
    // `self.parse::<T>()` parses as a generic_function; the callee must still be
    // the self-qualified method, not the stringified turbofish.
    let src = "pub struct P;\n\
               impl P {\n\
               \x20   pub fn run(&self) { self.parse::<u8>(); }\n\
               \x20   pub fn parse<T>(&self) {}\n\
               }\n";
    let docs = crate::rust::extract_documents_inner(std::path::Path::new("src/p.rs"), src)
        .expect("parse should succeed");
    let run = listing_text(&docs, "P::run");
    assert!(
        run.contains("edge::calls[self.parse]"),
        "self-method call with turbofish must emit `self.parse`; got: {run}"
    );
}

#[test]
fn rust_non_self_method_call_keeps_bare_field() {
    // A non-self receiver's type is unknown at parse time, so the bare method name
    // is kept (linker locality decides) — we must NOT invent a `self.` prefix.
    let src = "pub struct Engine;\n\
               impl Engine {\n\
               \x20   pub fn run(&self, other: &Engine) { other.flush(); }\n\
               \x20   pub fn flush(&self) {}\n\
               }\n";
    let docs = crate::rust::extract_documents_inner(std::path::Path::new("src/engine.rs"), src)
        .expect("parse should succeed");
    let run = listing_text(&docs, "Engine::run");
    assert!(
        run.contains("edge::calls[flush]") && !run.contains("edge::calls[self.flush]"),
        "non-self receiver must keep the bare method name; got: {run}"
    );
}

#[test]
fn rust_inherent_impl_emits_no_implements_macro() {
    let docs = crate::rust::extract_documents_inner(
        std::path::Path::new("src/greeter.rs"),
        RUST_TRAIT_FIXTURE,
    )
    .expect("parse should succeed");
    let shout = listing_text(&docs, "English::shout");
    assert!(
        !shout.contains("edge::implements["),
        "inherent-impl method must NOT emit implements; got: {shout}"
    );
}

#[test]
fn rust_mut_self_receiver_emits_mutates_macro() {
    let docs = crate::rust::extract_documents_inner(
        std::path::Path::new("src/greeter.rs"),
        RUST_TRAIT_FIXTURE,
    )
    .expect("parse should succeed");
    let shout = listing_text(&docs, "English::shout");
    assert!(
        shout.contains("edge::mutates[English]"),
        "`&mut self` method must emit edge::mutates[Type]; got: {shout}"
    );
    // Shared-reference receiver must not claim mutation.
    let whisper = listing_text(&docs, "English::whisper");
    assert!(
        !whisper.contains("edge::mutates["),
        "`&self` method must NOT emit mutates; got: {whisper}"
    );
    let greet = listing_text(&docs, "English::greet");
    assert!(
        !greet.contains("edge::mutates["),
        "`&self` trait method must NOT emit mutates; got: {greet}"
    );
}

// ── edge::imports emission ───────────────────────────────────────────────────
//
// Each language resolver must emit a file-level Module document carrying
// `edge::imports[target]` macros for module-level import statements.
//
// Target-string conventions:
//   Rust  : full qualified path per import item; `use foo::{a,b}` → `foo::a`, `foo::b`
//   Python: `import mod` → `mod`; `from pkg import sym` → `pkg.sym`
//   Go    : raw import path string (quotes stripped)
//   TS    : unique source_path per import statement (quotes stripped)

/// Helper: collect all Listing-block text from the *first* document whose
/// anchor ends with `suffix`.
fn imports_listing_text(docs: &[aden_core::Document], anchor_suffix: &str) -> String {
    let doc = docs
        .iter()
        .find(|d| d.anchor.ends_with(anchor_suffix))
        .unwrap_or_else(|| {
            panic!(
                "no document with anchor ending '{}'; got: {:?}",
                anchor_suffix,
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    doc.blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Rust ─────────────────────────────────────────────────────────────────────

/// `use foo::bar;` at module scope must emit `edge::imports[foo::bar]` on the
/// file-level Module document.
///
/// Note: when run inside the aden-parse crate tree, `src/lib.rs` resolves to
/// the actual crate src directory, so the crate name is "aden-parse" and the
/// module-entry-mapped file component is "src". The file-level doc anchor
/// therefore ends with `src#`.
#[test]
fn rust_simple_use_emits_imports_edge() {
    let src = r#"
use foo::bar;

pub fn f() {}
"#;
    let docs = crate::rust::extract_documents_inner(Path::new("src/lib.rs"), src)
        .expect("parse should succeed");
    // The file-level doc anchor ends with the mapped file component + "#".
    // For lib.rs inside aden-parse/src/, the file component maps to "src".
    let file_doc = docs
        .iter()
        .find(|d| d.anchor.ends_with('#'))
        .unwrap_or_else(|| {
            panic!(
                "expected a file-level document (anchor ending '#'); got: {:?}",
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    let listing: String = file_doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        listing.contains("edge::imports[foo::bar]"),
        "`use foo::bar` must emit edge::imports[foo::bar]; got: {listing}"
    );
}

/// `use foo::{a, b};` must emit one edge per item: `foo::a` and `foo::b`.
#[test]
fn rust_grouped_use_emits_imports_edges_per_item() {
    let src = r#"
use foo::{alpha, beta};

pub fn f() {}
"#;
    let docs = crate::rust::extract_documents_inner(Path::new("src/lib.rs"), src)
        .expect("parse should succeed");
    let file_doc = docs
        .iter()
        .find(|d| d.anchor.ends_with('#'))
        .unwrap_or_else(|| {
            panic!(
                "expected a file-level document; got: {:?}",
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    let listing: String = file_doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        listing.contains("edge::imports[foo::alpha]"),
        "`use foo::{{alpha, beta}}` must emit edge::imports[foo::alpha]; got: {listing}"
    );
    assert!(
        listing.contains("edge::imports[foo::beta]"),
        "`use foo::{{alpha, beta}}` must emit edge::imports[foo::beta]; got: {listing}"
    );
}

/// `use foo as local;` must emit the module path, not the alias.
#[test]
fn rust_use_as_emits_module_path_not_alias() {
    let src = r#"
use external_crate as ec;

pub fn f() {}
"#;
    let docs = crate::rust::extract_documents_inner(Path::new("src/lib.rs"), src)
        .expect("parse should succeed");
    let file_doc = docs
        .iter()
        .find(|d| d.anchor.ends_with('#'))
        .unwrap_or_else(|| {
            panic!(
                "expected a file-level document; got: {:?}",
                docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
            )
        });
    let listing: String = file_doc
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        listing.contains("edge::imports[external_crate]"),
        "`use external_crate as ec` must emit edge::imports[external_crate]; got: {listing}"
    );
    assert!(
        !listing.contains("edge::imports[ec]"),
        "alias 'ec' must NOT appear as import target; got: {listing}"
    );
}

// ── Python ───────────────────────────────────────────────────────────────────

/// `import os` must emit `edge::imports[os]`.
#[test]
fn python_bare_import_emits_imports_edge() {
    let src = "import os\n\ndef f():\n    pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("main.py"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "main.py#");
    assert!(
        listing.contains("edge::imports[os]"),
        "`import os` must emit edge::imports[os]; got: {listing}"
    );
}

/// `import os.path` must emit `edge::imports[os.path]`.
#[test]
fn python_dotted_import_emits_imports_edge() {
    let src = "import os.path\n\ndef f():\n    pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("main.py"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "main.py#");
    assert!(
        listing.contains("edge::imports[os.path]"),
        "`import os.path` must emit edge::imports[os.path]; got: {listing}"
    );
}

/// `from collections import OrderedDict` must emit `edge::imports[collections.OrderedDict]`.
#[test]
fn python_from_import_emits_qualified_imports_edge() {
    let src = "from collections import OrderedDict\n\ndef f():\n    pass\n";
    let resolver = crate::python_resolver::PythonResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("main.py"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "main.py#");
    assert!(
        listing.contains("edge::imports[collections.OrderedDict]"),
        "`from collections import OrderedDict` must emit edge::imports[collections.OrderedDict]; got: {listing}"
    );
}

// ── Go ───────────────────────────────────────────────────────────────────────

/// `import "fmt"` must emit `edge::imports[fmt]`.
#[test]
fn go_single_import_emits_imports_edge() {
    let src = r#"package main

import "fmt"

func main() {}
"#;
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("main.go"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "main.go#");
    assert!(
        listing.contains("edge::imports[fmt]"),
        "`import \"fmt\"` must emit edge::imports[fmt]; got: {listing}"
    );
}

/// Grouped `import ( "fmt"; "net/http" )` must emit both edges.
#[test]
fn go_grouped_import_emits_imports_edges() {
    let src = r#"package main

import (
    "fmt"
    "net/http"
)

func main() {}
"#;
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("main.go"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "main.go#");
    assert!(
        listing.contains("edge::imports[fmt]"),
        "grouped import must emit edge::imports[fmt]; got: {listing}"
    );
    assert!(
        listing.contains("edge::imports[net/http]"),
        "grouped import must emit edge::imports[net/http]; got: {listing}"
    );
}

// ── TypeScript ───────────────────────────────────────────────────────────────

/// `import { x } from './mod'` must emit `edge::imports[./mod]`.
#[test]
fn ts_named_import_emits_imports_edge() {
    let src = "import { x } from './mod';\n\nexport function f() {}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/index.ts"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "index.ts#");
    assert!(
        listing.contains("edge::imports[./mod]"),
        "`import {{ x }} from './mod'` must emit edge::imports[./mod]; got: {listing}"
    );
}

/// `import D from 'pkg'` must emit `edge::imports[pkg]`.
#[test]
fn ts_default_import_emits_imports_edge() {
    let src = "import D from 'pkg';\n\nexport function f() {}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/index.ts"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "index.ts#");
    assert!(
        listing.contains("edge::imports[pkg]"),
        "`import D from 'pkg'` must emit edge::imports[pkg]; got: {listing}"
    );
}

/// Multiple named imports from the same source must emit only ONE edge for
/// that source path (no duplicate `edge::imports` macros).
#[test]
fn ts_multiple_imports_same_source_emits_one_edge() {
    let src = "import { a, b } from './utils';\n\nexport function f() {}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/index.ts"))
        .expect("parse should succeed");
    let listing = imports_listing_text(&docs, "index.ts#");
    let count = listing.matches("edge::imports[./utils]").count();
    assert_eq!(
        count, 1,
        "two named imports from same source must emit exactly 1 edge; listing: {listing}"
    );
}

// ── TypeScript positive extraction smoke tests ───────────────────────────────
//
// These exercise real extraction paths that were previously uncovered:
//   - class + interface → NodeType::Type documents (distinct from Function)
//   - JSDoc `/** ... */` comment captured as a Paragraph block
//   - intra-file call site → `edge::calls[callee]` Listing block
//
// Anchors are asserted with `contains` (not exact match) because the full
// anchor embeds an inferred project path that varies by working directory.

/// A `class Foo` and an `interface IBar` must each produce a `NodeType::Type`
/// document. The class methods are also extracted as Function documents, but
/// the primary assertion is the Type-level presence of both named symbols.
#[test]
fn ts_class_and_interface_smoke() {
    let src = "class Foo {\n    doThing(): void {}\n}\n\ninterface IBar {\n    doSomething(): string;\n}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/shapes.ts"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    // Both Foo (class) and IBar (interface) must be present as Type documents.
    assert_has_anchor(&docs, "Foo");
    assert_has_anchor(&docs, "IBar");
    // Both must be NodeType::Type (not Function).
    // Anchors have the form `aden://...#Foo`; find by exact suffix to avoid
    // matching the method document `#Foo.doThing`.
    let foo = docs
        .iter()
        .find(|d| d.anchor.ends_with("#Foo"))
        .expect("Foo class document (anchor ending with #Foo)");
    assert_eq!(
        foo.node_type,
        aden_core::NodeType::Type,
        "class Foo must be NodeType::Type; got {:?}",
        foo.node_type
    );
    let ibar = docs
        .iter()
        .find(|d| d.anchor.ends_with("#IBar"))
        .expect("IBar interface document (anchor ending with #IBar)");
    assert_eq!(
        ibar.node_type,
        aden_core::NodeType::Type,
        "interface IBar must be NodeType::Type; got {:?}",
        ibar.node_type
    );
}

/// A function preceded by a `/** ... */` JSDoc comment must produce a Function
/// document whose blocks include a Paragraph containing the JSDoc text.
///
/// The TypeScript resolver captures the preceding comment via
/// `extract_ts_doc_comment`, which matches `/**`-prefixed siblings and stores
/// the raw comment text in `TsSymbol::doc_comment`. `emit_ts_symbol` then
/// pushes it as `Block::Paragraph`.
#[test]
fn ts_jsdoc_captured() {
    let src = "/** Greets a user by name. */\nfunction greet(name: string): void {}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/greet.ts"))
        .expect("parse should succeed");
    assert_has_anchor(&docs, "greet");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
    let greet_doc = docs
        .iter()
        .find(|d| d.anchor.contains("greet"))
        .expect("greet document");
    let has_jsdoc = greet_doc.blocks.iter().any(|b| match b {
        aden_core::Block::Paragraph(text) => text.contains("Greets a user by name"),
        _ => false,
    });
    assert!(
        has_jsdoc,
        "expected JSDoc text in a Paragraph block; blocks: {:?}",
        greet_doc.blocks
    );
}

/// When function A calls function B in the same file, A's document must contain
/// a `Listing` block with `edge::calls[beta]`. This is the TS analogue of the
/// existing `python_resolver_call_sites` test.
#[test]
fn ts_call_site_edge() {
    let src = "function alpha(): void {\n    beta();\n}\n\nfunction beta(): void {}\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/mod.ts"))
        .expect("parse should succeed");
    assert_has_anchor(&docs, "alpha");
    assert_has_anchor(&docs, "beta");
    let alpha = docs
        .iter()
        .find(|d| d.anchor.contains("alpha"))
        .expect("alpha document");
    let calls_text: String = alpha
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        calls_text.contains("edge::calls[beta]"),
        "expected `edge::calls[beta]` in alpha's Listing blocks; got: {calls_text}"
    );
}

#[test]
fn ts_this_method_call_emits_class_qualified_callee() {
    // `this.flush()` inside a class is rewritten to `Engine.flush` at parse time,
    // so the method caller gets a resolvable Calls edge (OO blast radius).
    let src = "class Engine {\n\
               \x20 run(): void { this.flush(); }\n\
               \x20 flush(): void {}\n\
               }\n";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("src/engine.ts"))
        .expect("parse should succeed");
    let run = docs
        .iter()
        .find(|d| d.anchor.contains("run"))
        .expect("run document");
    let calls_text: String = run
        .blocks
        .iter()
        .filter_map(|b| match b {
            aden_core::Block::Listing { code, .. } => Some(code.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        calls_text.contains("edge::calls[Engine.flush]"),
        "this-method call must emit class-qualified `Engine.flush`; got: {calls_text}"
    );
}

// ── Go positive extraction smoke tests ───────────────────────────────────────
//
// Three tests covering paths not exercised by the existing pointer-receiver
// regression guard: standalone function with doc comment, struct type, and
// interface type.
//
// NOTE: The Go resolver does NOT extract individual interface methods or struct
// fields as separate documents — an entire interface or struct is one Type
// document. Call sites within the function body produce `edge::calls[callee]`
// Listing blocks. Doc comments appear as `Block::Paragraph` with the raw
// `// ...` text preserved.

/// A standalone `func Compute(...)` with a `// Compute does X` doc comment
/// must produce a Function document whose blocks include the doc comment text.
#[test]
fn go_func_smoke() {
    let src =
        "package compute\n\n// Compute does X\nfunc Compute(x int) int {\n    return x * 2\n}\n";
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("pkg/compute.go"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "Compute");
    assert_has_node_type(&docs, aden_core::NodeType::Function);
    let compute = docs
        .iter()
        .find(|d| d.anchor.contains("Compute"))
        .expect("Compute document");
    let has_doc = compute.blocks.iter().any(|b| match b {
        aden_core::Block::Paragraph(text) => text.contains("Compute does X"),
        _ => false,
    });
    assert!(
        has_doc,
        "expected doc comment text in a Paragraph block; blocks: {:?}",
        compute.blocks
    );
}

/// `type Point struct { X int; Y int }` must produce a single `NodeType::Type`
/// document for `Point`.
///
/// NOTE: The Go resolver does NOT emit separate documents for struct fields
/// (X, Y). The entire struct is represented as one Type document. Field types
/// appear only as `edge::uses[T]` Listing blocks when the field type is a
/// user-defined (PascalCase) type — primitive types like `int` are filtered.
#[test]
fn go_struct_smoke() {
    let src = "package geometry\n\ntype Point struct {\n    X int\n    Y int\n}\n";
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("pkg/geometry.go"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "Point");
    let point = docs
        .iter()
        .find(|d| d.anchor.contains("Point"))
        .expect("Point document");
    assert_eq!(
        point.node_type,
        aden_core::NodeType::Type,
        "type Point struct must be NodeType::Type; got {:?}",
        point.node_type
    );
    // The resolver emits a "Go type from module ..." prose block.
    let has_prose = point.blocks.iter().any(|b| match b {
        aden_core::Block::Paragraph(text) => text.contains("Go type from module"),
        _ => false,
    });
    assert!(
        has_prose,
        "expected a 'Go type from module' Paragraph block; blocks: {:?}",
        point.blocks
    );
}

/// `type Shape interface { Area() float64 }` must produce a single
/// `NodeType::Type` document for `Shape`.
///
/// NOTE: The Go resolver treats interfaces the same as structs at the
/// extraction level — the whole interface is one Type document. The interface
/// method `Area` is NOT extracted as a separate Function document (Go method
/// declarations on concrete receiver types ARE extracted, but interface method
/// signatures inside an `interface_type` body are not walked by the resolver).
#[test]
fn go_interface_method() {
    let src = "package shapes\n\ntype Shape interface {\n    Area() float64\n}\n";
    let resolver = crate::go_resolver::GoResolver::new();
    let docs = resolver
        .extract_documents(src, Path::new("pkg/shapes.go"))
        .expect("parse should succeed");
    assert!(!docs.is_empty(), "expected non-empty document list");
    assert_has_anchor(&docs, "Shape");
    let shape = docs
        .iter()
        .find(|d| d.anchor.contains("Shape"))
        .expect("Shape document");
    assert_eq!(
        shape.node_type,
        aden_core::NodeType::Type,
        "type Shape interface must be NodeType::Type; got {:?}",
        shape.node_type
    );
    // Interface method `Area` is NOT a separate document in the current resolver.
    // Only the enclosing interface type is emitted.
    assert!(
        !docs.iter().any(|d| d.anchor.contains("Area")),
        "interface method Area must NOT produce a separate document (current resolver limitation); \
         got anchors: {:?}",
        docs.iter().map(|d| &d.anchor).collect::<Vec<_>>()
    );
}

// ── TypeScript async detection ────────────────────────────────────────────────
//
// `function_declaration` nodes are async iff they carry an unnamed `async`
// child token (kind = "async"). The old code used `node.kind() ==
// "function_declaration"` which is always true, marking every function async.

/// Returns true if the document carries an "Async" row in any Block::Table.
fn doc_is_async(doc: &aden_core::Document) -> bool {
    doc.blocks.iter().any(|b| match b {
        aden_core::Block::Table(t) => t
            .rows
            .iter()
            .any(|row| row.first().map(|s| s == "Async").unwrap_or(false)),
        _ => false,
    })
}

/// A plain synchronous function must NOT be marked async.
#[test]
fn ts_sync_function_not_async() {
    let src = "function g() {}";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, std::path::Path::new("src/mod.ts"))
        .expect("parse should succeed");
    let g = docs
        .iter()
        .find(|d| d.anchor.contains("g"))
        .expect("function g document");
    assert!(
        !doc_is_async(g),
        "sync `function g()` must NOT be marked async; blocks: {:?}",
        g.blocks
    );
}

/// An `async function` declaration must be marked async.
#[test]
fn ts_async_function_detected() {
    let src = "async function f() {}";
    let resolver = crate::typescript_resolver::TypeScriptResolver::new();
    let docs = resolver
        .extract_documents(src, std::path::Path::new("src/mod.ts"))
        .expect("parse should succeed");
    let f = docs
        .iter()
        .find(|d| d.anchor.contains("f"))
        .expect("function f document");
    assert!(
        doc_is_async(f),
        "`async function f()` must be marked async; blocks: {:?}",
        f.blocks
    );
}

// ── Source-link coverage: content nodes must carry a span ───────────────────
//
// Regression guard. Every consumer of node location (viz/grep/asm/understand)
// reads the `source_file` + `start_line` + `end_line` ATTRIBUTES and drops any
// node missing the full triple. Content nodes (whole-file docs/tables and
// doc-embedded code blocks) used to be emitted with `span: None`, so they had a
// file but no lines and silently rendered with no code link. These assert the
// triple is present so the regression cannot return.

/// True when a doc carries the full locatable attribute triple AND the typed
/// `source_span` field (the two channels must agree).
fn is_locatable(doc: &aden_core::Document) -> bool {
    doc.attributes.contains_key("source_file")
        && doc.attributes.contains_key("start_line")
        && doc.attributes.contains_key("end_line")
        && doc.source_span.is_some()
}

#[test]
fn plaintext_node_is_locatable() {
    let src = "line one\nline two\nline three\n";
    let docs = crate::plaintext::PlainTextExtractor::new()
        .extract_documents(src, Path::new("notes/readme.txt"))
        .expect("parse should succeed");
    let doc = docs.first().expect("one plaintext document");
    assert!(
        is_locatable(doc),
        "plaintext node must carry source_file/start_line/end_line + source_span; attrs: {:?}",
        doc.attributes
    );
    assert_eq!(
        doc.attributes.get("start_line").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        doc.attributes.get("end_line").map(String::as_str),
        Some("3")
    );
}

#[test]
fn plaintext_splits_paragraphs_into_notes() {
    // Three blank-line-separated paragraphs become three independent `Note`
    // nodes, each with its own line span — the notepad-prose granularity, so a
    // file of distinct thoughts is independently locatable, not one blob.
    let src =
        "First thought here.\n\nSecond, on another line.\nStill the second.\n\nThird and last.\n";
    let docs = crate::plaintext::PlainTextExtractor::new()
        .extract_documents(src, Path::new("notes/brain.txt"))
        .expect("parse should succeed");

    assert_eq!(docs.len(), 3, "one Note node per blank-line paragraph");
    assert!(
        docs.iter().all(|doc| doc.anchor.starts_with("aden://doc/")),
        "plain-text/RST notes must use the prose routing scheme"
    );
    assert!(
        docs.iter()
            .all(|d| d.node_type == aden_core::NodeType::Note),
        "each paragraph node is a Note"
    );

    // Per-paragraph anchors must be distinct.
    let anchors: std::collections::HashSet<&str> = docs.iter().map(|d| d.anchor.as_str()).collect();
    assert_eq!(anchors.len(), 3, "paragraph anchors must be distinct");

    // Line spans: para 1 = line 1, para 2 = lines 3-4, para 3 = line 6.
    let spans: Vec<(Option<&str>, Option<&str>)> = docs
        .iter()
        .map(|d| {
            (
                d.attributes.get("start_line").map(String::as_str),
                d.attributes.get("end_line").map(String::as_str),
            )
        })
        .collect();
    assert_eq!(spans[0], (Some("1"), Some("1")));
    assert_eq!(spans[1], (Some("3"), Some("4")));
    assert_eq!(spans[2], (Some("6"), Some("6")));

    // Every paragraph node carries source_file/start_line/end_line + source_span.
    assert!(
        docs.iter().all(is_locatable),
        "every paragraph node must be locatable"
    );
}

#[test]
fn csv_node_is_locatable() {
    let src = "name,age\nalice,30\nbob,40\n";
    let docs = crate::csv::CsvExtractor::new()
        .extract_documents(src, Path::new("data/people.csv"))
        .expect("parse should succeed");
    let doc = docs.first().expect("one csv document");
    assert!(
        is_locatable(doc),
        "csv node must carry source_file/start_line/end_line + source_span; attrs: {:?}",
        doc.attributes
    );
    assert_eq!(
        doc.attributes.get("end_line").map(String::as_str),
        Some("3")
    );
}

#[test]
fn asciidoc_docroot_and_codeblock_are_locatable() {
    // No headings → the whole file is one document node; plus a listing block.
    let src = "Some intro prose.\n\n----\nlet x = 1;\nlet y = 2;\n----\n";
    let docs = crate::asciidoc::AsciiDocExtractor::new()
        .extract_documents(src, Path::new("docs/guide.adoc"))
        .expect("parse should succeed");
    let root = docs
        .iter()
        .find(|d| d.anchor.ends_with("#document"))
        .expect("doc-root node");
    assert!(
        is_locatable(root),
        "asciidoc doc-root must be locatable; attrs: {:?}",
        root.attributes
    );
    let code = docs
        .iter()
        .find(|d| matches!(d.node_type, aden_core::NodeType::Script))
        .expect("code-block node");
    assert!(
        is_locatable(code),
        "asciidoc code block must be locatable; attrs: {:?}",
        code.attributes
    );
    // The block's span must point at the real fence body, not line 1.
    assert_eq!(
        code.attributes.get("start_line").map(String::as_str),
        Some("4"),
        "code block starts at its first body line, not the file top"
    );
}

#[test]
fn markdown_docroot_and_codeblock_are_locatable() {
    let src = "Intro paragraph with no heading.\n\n```rust\nlet x = 1;\n```\n";
    let docs = crate::markdown::MarkdownExtractor::new()
        .extract_documents(src, Path::new("docs/guide.md"))
        .expect("parse should succeed");
    let root = docs
        .iter()
        .find(|d| d.anchor.ends_with("#document"))
        .expect("doc-root node");
    assert!(
        is_locatable(root),
        "markdown doc-root must be locatable; attrs: {:?}",
        root.attributes
    );
    let code = docs
        .iter()
        .find(|d| matches!(d.node_type, aden_core::NodeType::Script))
        .expect("code-block node");
    assert!(
        is_locatable(code),
        "markdown code block must be locatable; attrs: {:?}",
        code.attributes
    );
}
