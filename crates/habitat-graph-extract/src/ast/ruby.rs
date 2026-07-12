//! Ruby AST extractor (`tree-sitter-ruby`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class nodes
//! `B_<class>`; module nodes `B_<module>`; top-level standalone method nodes `B_<method>`;
//! class/module method nodes `B_<class>_<method>` (instance and singleton methods alike).
//! Edges: `contains` (file→class/module/standalone-method), `method` (class/module→method),
//! `inherits` (superclass→class, local/external split), `imports_from` (file→require path).
//! `calls` and `uses` are deliberately **NOT** emitted — name-resolution heuristics would not
//! match graphify.
//!
//! Inheritance (`< SuperClass`) is extracted for `constant` (plain single-segment) superclasses
//! only; `scope_resolution` bases (`Mod::Class`) are silently skipped. Local/external split
//! mirrors graphify: a superclass declared in the same file gets `"b_<super>"` as the base id;
//! an external one gets `"<super>"`.
//!
//! `require` and `require_relative` calls emit `imports_from` edges. Only plain string-literal
//! arguments are extracted; variable arguments and computed strings are silently skipped.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

/// Extracts nodes and edges from Ruby source using `tree-sitter-ruby`, in graphify's
/// qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (lowercased file stem): one file node `B`; class nodes
/// `B_<class>`; module nodes `B_<module>`; top-level method nodes `B_<method>`; class/module
/// method nodes `B_<class>_<method>`. Edges: `contains` (file→class/module/method), `method`
/// (class/module→method), `inherits` (superclass→class, local/external), `imports_from`
/// (file→require path). `calls`/`uses` are deliberately omitted.
///
/// An empty file produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct RubyExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Returns the method component `m` of the qualified id `B_C_m`.
///
/// Strips all leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. `___` → `"___"`).
///
/// Examples: `initialize` → `"initialize"`, `__id__` → `"id"`, `_private` → `"private"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Extracts a lowercased path string from a tree-sitter-ruby `string` node.
///
/// Tries the `string_content` named child first (gives clean content without delimiters);
/// falls back to stripping surrounding `'` or `"` quote characters from the node text.
/// Returns `None` for empty strings or non-`string` nodes.
fn string_path(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    // Try `string_content` named child — tree-sitter-ruby wraps content in this node.
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "string_content" {
            let text = text_of(source, &child);
            if !text.is_empty() {
                return Some(text.to_lowercase());
            }
        }
    }
    // Fallback: strip surrounding quote characters from the full string node text.
    let raw = text_of(source, node);
    let stripped = raw.trim_matches(|c: char| c == '\'' || c == '"');
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_lowercase())
    }
}

/// Extracts the lowercased `constant` text from a superclass field node.
///
/// Handles two tree-sitter-ruby representations:
/// - The `superclass` field is directly a `constant` node.
/// - The `superclass` field is a wrapping node whose first named child is `constant`.
///
/// Returns `None` for `scope_resolution` or other complex superclass expressions.
fn constant_text(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "constant" {
        return Some(text_of(source, node).to_lowercase());
    }
    // Walk one level deeper (handles any wrapper node in different grammar versions).
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "constant" {
            return Some(text_of(source, &child).to_lowercase());
        }
    }
    None
}

/// Locates the `body_statement` of a `class` or `module` node.
///
/// Tries the named `"body"` field first (exposed by some versions of tree-sitter-ruby); then
/// falls back to scanning named children for a `body_statement` node. Returns `None` for empty
/// class/module bodies.
fn find_body<'a>(node: &'a tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "body_statement" {
            return Some(child);
        }
    }
    None
}

/// Collects the lowercased names of all top-level `class` and `module` declarations.
///
/// Used in pass 1 to distinguish local from external superclasses when emitting `inherits`
/// edges.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "class" | "module") {
            if let Some(name_node) = child.child_by_field_name("name") {
                names.insert(text_of(source, &name_node).to_lowercase());
            }
        }
    }
    names
}

/// Extracts `method` and `singleton_method` children from a class/module body statement.
///
/// Emits a method node `B_<class>_<m>` and a `method` edge (from `class_label` to the node)
/// for each direct `method` or `singleton_method` child. Names are processed through
/// [`method_id`] (leading/trailing underscores stripped, then lowercased).
fn extract_body_methods(
    body: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    class_lower: &str,
    class_label: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for i in 0..body.named_child_count() {
        let Some(child) = body.named_child(i) else {
            continue;
        };
        let name_node = match child.kind() {
            "method" | "singleton_method" => child.child_by_field_name("name"),
            _ => continue,
        };
        let Some(name_node) = name_node else {
            continue;
        };
        let raw = text_of(source, &name_node);
        let m = method_id(&raw);
        let method_label = format!("{b}_{class_lower}_{m}");
        result.nodes.push(RawNode {
            label: method_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&child),
        });
        result.edges.push(RawEdge {
            source: class_label.to_owned(),
            target: method_label,
            relation: "method".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

/// Extracts a top-level `class` node into `result`.
///
/// Emits:
/// - A class node `B_<c>` (lowercased class name).
/// - A `contains` edge from `B` to the class node.
/// - One `inherits` edge if a plain-constant superclass is present: base id is `"b_<super>"`
///   for locally-declared superclasses and `"<super>"` for external ones.
/// - One method node `B_<c>_<m>` and one `method` edge per `method`/`singleton_method` in the
///   class body.
fn extract_class(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let c = text_of(source, &name_node).to_lowercase();
    let class_label = format!("{b}_{c}");

    result.nodes.push(RawNode {
        label: class_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: class_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // Inheritance: `superclass` field holds the parent class (a `constant` or wrapper).
    if let Some(sc_node) = node.child_by_field_name("superclass") {
        if let Some(parent_lower) = constant_text(&sc_node, source) {
            let base_id = if local_types.contains(&parent_lower) {
                format!("{b}_{parent_lower}")
            } else {
                parent_lower
            };
            result.edges.push(RawEdge {
                source: base_id,
                target: class_label.clone(),
                relation: "inherits".to_owned(),
                confidence: Confidence::Extracted,
            });
        }
    }

    // Methods: walk the class body_statement for method/singleton_method children.
    if let Some(body) = find_body(node) {
        extract_body_methods(&body, source, b, &c, &class_label, source_file, result);
    }
}

/// Extracts a top-level `module` node into `result`.
///
/// Emits a module node `B_<m>` and a `contains` edge from `B`, plus `method`/`singleton_method`
/// nodes and `method` edges from the module body. Modules cannot have superclasses, so no
/// `inherits` edge is emitted.
fn extract_module(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let m = text_of(source, &name_node).to_lowercase();
    let module_label = format!("{b}_{m}");

    result.nodes.push(RawNode {
        label: module_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: module_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    if let Some(body) = find_body(node) {
        extract_body_methods(&body, source, b, &m, &module_label, source_file, result);
    }
}

/// Extracts a standalone top-level `method` node (file-scoped `def`) into `result`.
///
/// Top-level `def` in Ruby defines methods on `Object` / the main object. We treat them as
/// file-scoped symbols, emitting a node `B_<fn>` and a `contains` edge from `B`.
fn extract_toplevel_method(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let raw = text_of(source, &name_node);
    let fn_lower = method_id(&raw);
    let fn_label = format!("{b}_{fn_lower}");

    result.nodes.push(RawNode {
        label: fn_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: fn_label,
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Processes a top-level `call` node, emitting an `imports_from` edge for `require` and
/// `require_relative` calls with a plain string-literal argument.
///
/// Non-require calls and calls with non-literal arguments are silently ignored.
fn extract_require(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    // `method` field must be a plain identifier named "require" or "require_relative".
    let Some(method_node) = node.child_by_field_name("method") else {
        return;
    };
    if method_node.kind() != "identifier" {
        return;
    }
    let method_text = text_of(source, &method_node);
    if method_text != "require" && method_text != "require_relative" {
        return;
    }
    // `arguments` field: first named child is the string literal.
    let Some(args_node) = node.child_by_field_name("arguments") else {
        return;
    };
    let Some(arg) = args_node.named_child(0) else {
        return;
    };
    if let Some(path) = string_path(&arg, source) {
        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: path,
            relation: "imports_from".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for RubyExtractor {
    fn language(&self) -> &'static str {
        "ruby"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rb"]
    }

    /// Extracts Ruby nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased); class
    /// nodes `B_<class>`; module nodes `B_<module>`; standalone method nodes `B_<method>`;
    /// class/module method nodes `B_<class>_<method>`; with `contains`, `method`, `inherits`,
    /// and `imports_from` edges. `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Ruby grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-ruby`).
    /// - `parser.parse` returns `None` (cancellation / timeout — not for invalid Ruby syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .map_err(|e| GraphError::Parse {
                file: source_file.clone(),
                message: e.to_string(),
            })?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| GraphError::Parse {
                file: source_file.clone(),
                message: "parse returned None".into(),
            })?;

        let root = tree.root_node();

        // Pass 1: collect local class/module names for the inherits local/external split.
        let local_types = local_type_names(root, source);

        let mut result = Extraction::new();

        // Always emit the file node, even for empty source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Pass 2: walk top-level `program` children, dispatching per node kind.
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            match child.kind() {
                "class" => {
                    extract_class(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "module" => {
                    extract_module(&child, source, &b, &source_file, &mut result);
                }
                "method" => {
                    extract_toplevel_method(&child, source, &b, &source_file, &mut result);
                }
                "call" => {
                    extract_require(&child, source, &b, &mut result);
                }
                _ => {}
            }
        }

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::RubyExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        RubyExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("ruby extractor failed on {filename}: {e}"))
    }

    /// Returns `true` if `ex` contains a node with the given label.
    fn has_node(ex: &Extraction, label: &str) -> bool {
        ex.nodes.iter().any(|n| n.label == label)
    }

    /// Returns `true` if `ex` contains an edge with matching source, target, and relation.
    fn has_edge(ex: &Extraction, src: &str, tgt: &str, rel: &str) -> bool {
        ex.edges
            .iter()
            .any(|e| e.source == src && e.target == tgt && e.relation == rel)
    }

    /// Returns the node with the given label, panicking if absent.
    fn node<'e>(ex: &'e Extraction, label: &str) -> &'e habitat_graph_core::RawNode {
        ex.nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "node '{label}' not found; got: {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    // ── 1. Empty / minimal source ─────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_exactly_one_file_node() {
        let ex = extract("", "empty.rb");
        assert_eq!(ex.nodes.len(), 1, "empty source: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
    }

    #[test]
    fn empty_source_yields_no_edges() {
        let ex = extract("", "empty.rb");
        assert_eq!(ex.edges.len(), 0, "empty source: expected 0 edges");
    }

    #[test]
    fn comment_only_source_yields_only_file_node() {
        let ex = extract("# just a comment\n", "comments.rb");
        assert_eq!(ex.nodes.len(), 1);
        assert_eq!(ex.nodes[0].label, "comments");
        assert_eq!(ex.edges.len(), 0);
    }

    // ── 2. File stem ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_lowercased() {
        let ex = extract("", "User.rb");
        assert_eq!(ex.nodes[0].label, "user");
    }

    #[test]
    fn file_stem_mixed_case_fully_lowercased() {
        let ex = extract("", "ActiveRecord.rb");
        assert_eq!(ex.nodes[0].label, "activerecord");
    }

    #[test]
    fn file_stem_with_underscores_preserved() {
        let ex = extract("", "my_model.rb");
        assert_eq!(ex.nodes[0].label, "my_model");
    }

    // ── 3. Class extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn single_class_emits_class_node() {
        let src = "class Dog\nend\n";
        let ex = extract(src, "dog.rb");
        assert!(has_node(&ex, "dog_dog"), "class node missing");
    }

    #[test]
    fn single_class_emits_contains_edge() {
        let src = "class Dog\nend\n";
        let ex = extract(src, "dog.rb");
        assert!(
            has_edge(&ex, "dog", "dog_dog", "contains"),
            "contains edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_name_lowercased() {
        let src = "class HTTPClient\nend\n";
        let ex = extract(src, "client.rb");
        assert!(
            has_node(&ex, "client_httpclient"),
            "class name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_classes_all_emitted() {
        let src = "class A\nend\nclass B\nend\nclass C\nend\n";
        let ex = extract(src, "abc.rb");
        assert!(has_node(&ex, "abc_a"), "A missing");
        assert!(has_node(&ex, "abc_b"), "B missing");
        assert!(has_node(&ex, "abc_c"), "C missing");
        let class_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "abc").collect();
        assert_eq!(class_nodes.len(), 3, "expected 3 class nodes");
    }

    #[test]
    fn class_without_methods_has_no_method_nodes() {
        let src = "class Empty\nend\n";
        let ex = extract(src, "empty.rb");
        // file node + class node = 2 nodes, no method nodes
        assert_eq!(ex.nodes.len(), 2);
        assert!(
            !ex.edges.iter().any(|e| e.relation == "method"),
            "empty class must have no method edges"
        );
    }

    // ── 4. Module extraction ──────────────────────────────────────────────────────────────────

    #[test]
    fn single_module_emits_module_node() {
        let src = "module Greetable\nend\n";
        let ex = extract(src, "greetable.rb");
        assert!(has_node(&ex, "greetable_greetable"), "module node missing");
    }

    #[test]
    fn single_module_emits_contains_edge() {
        let src = "module Greetable\nend\n";
        let ex = extract(src, "greetable.rb");
        assert!(has_edge(
            &ex,
            "greetable",
            "greetable_greetable",
            "contains"
        ));
    }

    #[test]
    fn module_name_lowercased() {
        let src = "module ActiveRecord\nend\n";
        let ex = extract(src, "ar.rb");
        assert!(has_node(&ex, "ar_activerecord"));
    }

    #[test]
    fn multiple_modules_all_emitted() {
        let src = "module M1\nend\nmodule M2\nend\n";
        let ex = extract(src, "mods.rb");
        assert!(has_node(&ex, "mods_m1"));
        assert!(has_node(&ex, "mods_m2"));
    }

    // ── 5. Instance method extraction ─────────────────────────────────────────────────────────

    #[test]
    fn instance_method_emits_method_node() {
        let src = "class Dog\ndef bark\nend\nend\n";
        let ex = extract(src, "dog.rb");
        assert!(has_node(&ex, "dog_dog_bark"), "method node missing");
    }

    #[test]
    fn instance_method_emits_method_edge() {
        let src = "class Dog\ndef bark\nend\nend\n";
        let ex = extract(src, "dog.rb");
        assert!(
            has_edge(&ex, "dog_dog", "dog_dog_bark", "method"),
            "method edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn method_label_follows_b_class_method_format() {
        let src = "class Cat\ndef meow\nend\nend\n";
        let ex = extract(src, "cat.rb");
        // B="cat", class="cat", method="meow" → "cat_cat_meow"
        assert!(has_node(&ex, "cat_cat_meow"));
    }

    #[test]
    fn method_name_lowercased() {
        let src = "class Greeter\ndef SayHello\nend\nend\n";
        let ex = extract(src, "greeter.rb");
        assert!(
            has_node(&ex, "greeter_greeter_sayhello"),
            "method name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_methods_all_emitted() {
        let src = concat!(
            "class Animal\n",
            "def eat\nend\n",
            "def sleep\nend\n",
            "def breathe\nend\n",
            "end\n"
        );
        let ex = extract(src, "animal.rb");
        assert!(has_node(&ex, "animal_animal_eat"), "eat missing");
        assert!(has_node(&ex, "animal_animal_sleep"), "sleep missing");
        assert!(has_node(&ex, "animal_animal_breathe"), "breathe missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn initialize_method_processed_through_method_id() {
        // `initialize` has no leading/trailing underscores → stays "initialize"
        let src = "class Foo\ndef initialize\nend\nend\n";
        let ex = extract(src, "foo.rb");
        assert!(has_node(&ex, "foo_foo_initialize"));
    }

    // ── 6. Singleton method (class method) extraction ─────────────────────────────────────────

    #[test]
    fn singleton_method_emits_method_node() {
        let src = "class Factory\ndef self.create\nend\nend\n";
        let ex = extract(src, "factory.rb");
        assert!(
            has_node(&ex, "factory_factory_create"),
            "singleton method node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn singleton_method_emits_method_edge_from_class() {
        let src = "class Factory\ndef self.build\nend\nend\n";
        let ex = extract(src, "factory.rb");
        assert!(has_edge(
            &ex,
            "factory_factory",
            "factory_factory_build",
            "method"
        ));
    }

    #[test]
    fn class_with_both_instance_and_singleton_methods() {
        let src = concat!(
            "class Config\n",
            "def initialize\nend\n",
            "def self.load\nend\n",
            "end\n"
        );
        let ex = extract(src, "config.rb");
        assert!(
            has_node(&ex, "config_config_initialize"),
            "initialize missing"
        );
        assert!(
            has_node(&ex, "config_config_load"),
            "singleton load missing"
        );
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 2, "expected 2 method edges");
    }

    #[test]
    fn singleton_method_in_module() {
        let src = "module Helper\ndef self.format\nend\nend\n";
        let ex = extract(src, "helper.rb");
        assert!(has_node(&ex, "helper_helper_format"));
        assert!(has_edge(
            &ex,
            "helper_helper",
            "helper_helper_format",
            "method"
        ));
    }

    // ── 7. Method-id stripping ────────────────────────────────────────────────────────────────

    #[test]
    fn method_with_leading_underscore_stripped() {
        // `_private_method` → "private_method"
        let src = "class C\ndef _private_method\nend\nend\n";
        let ex = extract(src, "c.rb");
        assert!(
            has_node(&ex, "c_c_private_method"),
            "leading _ must be stripped; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_with_dunder_underscores_stripped() {
        // `__id__` → "id"
        let src = "class C\ndef __id__\nend\nend\n";
        let ex = extract(src, "c.rb");
        assert!(
            has_node(&ex, "c_c_id"),
            "__id__ must strip to 'id'; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_name_all_underscores_not_stripped_to_empty() {
        // `___` → stays "___" (fallback: raw lowercased)
        let src = "class C\ndef ___\nend\nend\n";
        let ex = extract(src, "c.rb");
        // method_id("___") = "___" (not stripped to empty)
        assert!(
            has_node(&ex, "c_c____"),
            "all-underscore method name must not become empty; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 8. Inheritance ────────────────────────────────────────────────────────────────────────

    #[test]
    fn class_with_external_superclass_emits_inherits_edge() {
        let src = "class Dog < Animal\nend\n";
        let ex = extract(src, "dog.rb");
        // Animal is external (not declared in this file) → base_id = "animal"
        assert!(
            has_edge(&ex, "animal", "dog_dog", "inherits"),
            "inherits from external 'animal' missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_without_superclass_has_no_inherits_edge() {
        let src = "class Standalone\nend\n";
        let ex = extract(src, "standalone.rb");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "inherits"),
            "no inherits edge expected for class without superclass"
        );
    }

    #[test]
    fn local_superclass_gets_prefixed_base_id() {
        let src = "class Animal\nend\nclass Dog < Animal\nend\n";
        let ex = extract(src, "animals.rb");
        // Animal IS declared locally → base_id = "animals_animal"
        assert!(
            has_edge(&ex, "animals_animal", "animals_dog", "inherits"),
            "local superclass must use prefixed id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn external_superclass_has_no_prefix() {
        let src = "class Poodle < Dog\nend\n";
        let ex = extract(src, "poodle.rb");
        // Dog is external → base_id = "dog" (no file-prefix)
        assert!(
            has_edge(&ex, "dog", "poodle_poodle", "inherits"),
            "external superclass must not be prefixed; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn inherits_edge_source_is_base_target_is_derived() {
        let src = "class Child < Parent\nend\n";
        let ex = extract(src, "child.rb");
        let inh: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        assert_eq!(inh.len(), 1, "expected 1 inherits edge");
        assert_eq!(inh[0].source, "parent", "source must be base");
        assert_eq!(inh[0].target, "child_child", "target must be derived");
    }

    #[test]
    fn multiple_classes_some_inheriting() {
        let src = concat!(
            "class Base\nend\n",
            "class Mid < Base\nend\n",
            "class Leaf < Mid\nend\n"
        );
        let ex = extract(src, "chain.rb");
        // Base declared locally → Mid inherits from "chain_base"
        assert!(has_edge(&ex, "chain_base", "chain_mid", "inherits"));
        // Mid declared locally → Leaf inherits from "chain_mid"
        assert!(has_edge(&ex, "chain_mid", "chain_leaf", "inherits"));
    }

    // ── 9. Require / imports_from ─────────────────────────────────────────────────────────────

    #[test]
    fn require_single_quotes_emits_imports_from_edge() {
        let src = "require 'rails'\n";
        let ex = extract(src, "app.rb");
        assert!(
            has_edge(&ex, "app", "rails", "imports_from"),
            "require 'rails' must emit imports_from; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn require_double_quotes_emits_imports_from_edge() {
        let src = "require \"active_record\"\n";
        let ex = extract(src, "models.rb");
        assert!(
            has_edge(&ex, "models", "active_record", "imports_from"),
            "require double-quote must emit imports_from; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn require_relative_emits_imports_from_edge() {
        let src = "require_relative './helpers'\n";
        let ex = extract(src, "app.rb");
        assert!(
            has_edge(&ex, "app", "./helpers", "imports_from"),
            "require_relative must emit imports_from; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn require_path_is_lowercased() {
        let src = "require 'ActiveRecord'\n";
        let ex = extract(src, "ar.rb");
        assert!(
            has_edge(&ex, "ar", "activerecord", "imports_from"),
            "require path must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_requires_all_emitted() {
        let src = concat!(
            "require 'rails'\n",
            "require 'json'\n",
            "require 'logger'\n"
        );
        let ex = extract(src, "app.rb");
        assert!(has_edge(&ex, "app", "rails", "imports_from"), "rails");
        assert!(has_edge(&ex, "app", "json", "imports_from"), "json");
        assert!(has_edge(&ex, "app", "logger", "imports_from"), "logger");
        let import_edges: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .collect();
        assert_eq!(import_edges.len(), 3, "expected 3 imports_from edges");
    }

    #[test]
    fn require_with_slashed_path() {
        let src = "require 'net/http'\n";
        let ex = extract(src, "fetch.rb");
        assert!(
            has_edge(&ex, "fetch", "net/http", "imports_from"),
            "slashed require path missing; edges: {:?}",
            ex.edges
        );
    }

    // ── 10. Top-level standalone methods ──────────────────────────────────────────────────────

    #[test]
    fn toplevel_method_emits_node_and_contains_edge() {
        let src = "def greet\nend\n";
        let ex = extract(src, "greet.rb");
        assert!(
            has_node(&ex, "greet_greet"),
            "standalone method node missing"
        );
        assert!(
            has_edge(&ex, "greet", "greet_greet", "contains"),
            "contains edge for standalone method missing"
        );
    }

    #[test]
    fn toplevel_method_name_lowercased() {
        let src = "def MyHelper\nend\n";
        let ex = extract(src, "helper.rb");
        assert!(has_node(&ex, "helper_myhelper"));
    }

    #[test]
    fn multiple_toplevel_methods_all_emitted() {
        let src = "def alpha\nend\ndef beta\nend\ndef gamma\nend\n";
        let ex = extract(src, "fns.rb");
        assert!(has_node(&ex, "fns_alpha"));
        assert!(has_node(&ex, "fns_beta"));
        assert!(has_node(&ex, "fns_gamma"));
    }

    // ── 11. Negative invariants ───────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = concat!(
            "class Foo\n",
            "def bar\n",
            "puts 'hello'\n",
            "end\n",
            "end\n"
        );
        let ex = extract(src, "noisy.rb");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "class X\ndef y\nx = 1\nend\nend\n";
        let ex = extract(src, "x.rb");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn non_require_calls_produce_no_edges() {
        // `puts 'hello'` must not emit any edge
        let src = "puts 'hello'\nsend(:foo)\n";
        let ex = extract(src, "side.rb");
        // Only the file node; zero edges expected (no require calls)
        assert_eq!(ex.nodes.len(), 1, "only file node expected");
        assert_eq!(ex.edges.len(), 0, "no edges expected for non-require calls");
    }

    // ── 12. Confidence + source_file ──────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "require 'rails'\n",
            "class Dog < Animal\n",
            "def bark\nend\n",
            "end\n"
        );
        let ex = extract(src, "dog.rb");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Confidence::Extracted; got {edge:?}"
            );
        }
    }

    #[test]
    fn source_file_field_contains_path_in_every_node() {
        let src = concat!("class Repo\ndef save\nend\nend\n");
        let ex = extract(src, "myrepo.rb");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("myrepo.rb"),
                "source_file {:?} must contain 'myrepo.rb'",
                n.source_file
            );
        }
    }

    // ── 13. Span correctness ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_start_byte_is_zero() {
        let src = "class Foo\nend\n";
        let ex = extract(src, "foo.rb");
        let file_node = node(&ex, "foo");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn file_node_span_start_line_is_one() {
        let src = "class Foo\nend\n";
        let ex = extract(src, "foo.rb");
        let file_node = node(&ex, "foo");
        assert_eq!(
            file_node.span.start_line, 1,
            "file node must start at line 1"
        );
    }

    #[test]
    fn class_node_span_is_well_formed() {
        let src = "class Dog\ndef bark\nend\nend\n";
        let ex = extract(src, "dog.rb");
        let n = node(&ex, "dog_dog");
        assert!(
            n.span.is_well_formed(),
            "class span must be well-formed: {n:?}"
        );
    }

    #[test]
    fn method_node_span_end_ge_start() {
        let src = concat!(
            "class Calc\n",
            "def add(a, b)\n",
            "a + b\n",
            "end\n",
            "end\n"
        );
        let ex = extract(src, "calc.rb");
        let n = node(&ex, "calc_calc_add");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    #[test]
    fn class_span_is_non_empty() {
        let src = "class Big\n  x = 1\nend\n";
        let ex = extract(src, "big.rb");
        let n = node(&ex, "big_big");
        assert!(!n.span.is_empty(), "class span must not be empty");
    }

    // ── 14. Totals / composites ───────────────────────────────────────────────────────────────

    #[test]
    fn contains_edge_count_matches_symbols_in_file() {
        let src = concat!(
            "class A\nend\n",
            "class B\nend\n",
            "module M\nend\n",
            "def fn1\nend\n"
        );
        let ex = extract(src, "totals.rb");
        let contains: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .collect();
        // A, B, M, fn1 = 4 contains edges
        assert_eq!(
            contains.len(),
            4,
            "expected 4 contains edges; got {contains:?}"
        );
    }

    #[test]
    fn method_edge_count_matches_method_declarations() {
        let src = concat!(
            "class X\n",
            "def m1\nend\n",
            "def m2\nend\n",
            "def self.m3\nend\n",
            "end\n"
        );
        let ex = extract(src, "x.rb");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn class_and_module_mixed_all_emitted() {
        let src = concat!(
            "module Concerns\n",
            "def self.helpers\nend\n",
            "end\n",
            "class Model\n",
            "def save\nend\n",
            "end\n"
        );
        let ex = extract(src, "mixed.rb");
        assert!(has_node(&ex, "mixed_concerns"), "module node");
        assert!(has_node(&ex, "mixed_model"), "class node");
        assert!(has_node(&ex, "mixed_concerns_helpers"), "module method");
        assert!(has_node(&ex, "mixed_model_save"), "class method");
    }

    #[test]
    fn two_classes_two_methods_each_produce_correct_node_count() {
        let src = concat!(
            "class A\n",
            "def x\nend\n",
            "def y\nend\n",
            "end\n",
            "class B\n",
            "def p\nend\n",
            "def q\nend\n",
            "end\n"
        );
        let ex = extract(src, "ab.rb");
        // file(1) + A(1) + B(1) + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 15. Extractor metadata ────────────────────────────────────────────────────────────────

    #[test]
    fn language_slug_is_ruby() {
        assert_eq!(RubyExtractor.language(), "ruby");
    }

    #[test]
    fn extension_is_rb() {
        assert_eq!(RubyExtractor.extensions(), &["rb"]);
    }

    // ── 16. Malformed / unusual source ────────────────────────────────────────────────────────

    #[test]
    fn malformed_ruby_does_not_panic() {
        // tree-sitter is error-tolerant and produces a partial tree for broken input.
        let src = "class ??? broken {{{ def end end\n";
        let ex = extract(src, "bad.rb");
        assert!(has_node(&ex, "bad"), "file node must always be present");
    }

    #[test]
    fn syntax_error_with_class_still_extracts_file_node() {
        let src = "class\n";
        let ex = extract(src, "broken.rb");
        assert_eq!(ex.nodes[0].label, "broken");
    }

    // ── 17. Realistic composite ───────────────────────────────────────────────────────────────

    #[test]
    fn realistic_ruby_model_file() {
        let src = concat!(
            "require 'active_record'\n",
            "require 'json'\n",
            "\n",
            "class ApplicationRecord\n",
            "def self.table_name\nend\n",
            "end\n",
            "\n",
            "class User < ApplicationRecord\n",
            "def initialize(name)\nend\n",
            "def full_name\nend\n",
            "def self.find_by_email\nend\n",
            "end\n"
        );
        let ex = extract(src, "user.rb");
        // File node
        assert!(has_node(&ex, "user"), "file node");
        // Classes
        assert!(has_node(&ex, "user_applicationrecord"), "ApplicationRecord");
        assert!(has_node(&ex, "user_user"), "User");
        // Methods on ApplicationRecord
        assert!(
            has_node(&ex, "user_applicationrecord_table_name"),
            "table_name"
        );
        // Methods on User
        assert!(has_node(&ex, "user_user_initialize"), "initialize");
        assert!(has_node(&ex, "user_user_full_name"), "full_name");
        assert!(has_node(&ex, "user_user_find_by_email"), "find_by_email");
        // Imports
        assert!(has_edge(&ex, "user", "active_record", "imports_from"));
        assert!(has_edge(&ex, "user", "json", "imports_from"));
        // Inheritance: User < ApplicationRecord (local → prefixed)
        assert!(has_edge(
            &ex,
            "user_applicationrecord",
            "user_user",
            "inherits"
        ));
        // Edges
        assert!(has_edge(&ex, "user", "user_applicationrecord", "contains"));
        assert!(has_edge(&ex, "user", "user_user", "contains"));
    }

    #[test]
    fn module_with_methods_realistic() {
        let src = concat!(
            "module Serializable\n",
            "def to_json\nend\n",
            "def from_json\nend\n",
            "def self.schema\nend\n",
            "end\n"
        );
        let ex = extract(src, "serializable.rb");
        assert!(has_node(&ex, "serializable_serializable"));
        assert!(has_node(&ex, "serializable_serializable_to_json"));
        assert!(has_node(&ex, "serializable_serializable_from_json"));
        assert!(has_node(&ex, "serializable_serializable_schema"));
        assert!(has_edge(
            &ex,
            "serializable_serializable",
            "serializable_serializable_to_json",
            "method"
        ));
        assert!(has_edge(
            &ex,
            "serializable_serializable",
            "serializable_serializable_schema",
            "method"
        ));
    }

    // ── 18. Edge-source invariants ────────────────────────────────────────────────────────────

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let src = "class Worker\ndef run\nend\nend\n";
        let ex = extract(src, "wk.rb");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 1);
        assert_eq!(
            method_edges[0].source, "wk_worker",
            "method edge source must be class label"
        );
        assert_ne!(
            method_edges[0].source, "wk",
            "method edge source must NOT be file label"
        );
    }

    #[test]
    fn contains_edge_source_is_always_file_label() {
        let src = "class X\nend\nmodule Y\nend\ndef z\nend\n";
        let ex = extract(src, "f.rb");
        for edge in ex.edges.iter().filter(|e| e.relation == "contains") {
            assert_eq!(
                edge.source, "f",
                "all contains edges must source from file label 'f'; got {edge:?}"
            );
        }
    }

    // ── 19. Unicode ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn unicode_in_source_does_not_panic() {
        // Embedding unicode in a comment or string; class name stays ASCII.
        let src = "# こんにちは\nclass Foo\ndef bar\nend\nend\n";
        let ex = extract(src, "uni.rb");
        assert!(has_node(&ex, "uni_foo"));
        assert!(has_node(&ex, "uni_foo_bar"));
    }

    // ── 20. File node always present ─────────────────────────────────────────────────────────

    #[test]
    fn file_node_always_present_for_any_source() {
        for src in &["", "# nothing", "class X; end", "def f; end"] {
            let ex = extract(src, "always.rb");
            assert!(
                has_node(&ex, "always"),
                "file node must always be present; src={src:?}"
            );
        }
    }
}
