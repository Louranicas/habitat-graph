//! C++ AST extractor (`tree-sitter-cpp`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (filename without extension, lowercased): one file node `B`;
//! top-level function nodes `B_<fn>`; class/struct nodes `B_<name>`; namespace nodes `B_<ns>`;
//! method nodes `B_<cls>_<method>`. Edges: `contains` (file→symbol, namespace→symbol), `method`
//! (class→method), `inherits` (base→derived, local/external split), `imports_from`
//! (file→header path). `calls` and `uses` are deliberately NOT emitted (name-resolution
//! heuristics that would not match graphify).
//!
//! C++ specifics handled:
//! - Destructors (`~Foo`) and operator overloads (`operator+`) are silently skipped — they have
//!   no stable simple-name form in graphify's taxonomy.
//! - Constructors (lowercased method name matches the class name) are skipped for the same
//!   reason.
//! - `#include <header.h>` and `#include "header.h"` both produce `imports_from` edges; angle
//!   brackets and quotes are stripped and the path is lowercased.
//! - Anonymous namespaces (no `name` field) are silently skipped.
//! - Forward declarations (`class Foo;`) parse as `declaration` nodes, not `class_specifier`,
//!   and are therefore not emitted — only fully-defined symbols appear.
//! - Template declarations wrapping classes or functions are silently skipped (template
//!   instantiation names are not stable qualified ids).
//!
//! An empty file produces exactly one node (the file node) and no edges.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes/edges from C++ source using `tree-sitter-cpp`, in **graphify's qualified-id
/// taxonomy** so the output is comparable to graphify's committed goldens.
///
/// For a file with basename `B` (filename without extension, lowercased): one file node `B`;
/// function nodes `B_<fn>`; class/struct nodes `B_<name>`; namespace nodes `B_<ns>`; method
/// nodes `B_<cls>_<method>`. Edges: `contains` (file→symbol, namespace→symbol), `method`
/// (class→method), `inherits` (base→derived, local/external split), `imports_from`
/// (file→header). `calls` and `uses` are deliberately omitted.
///
/// An empty file produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct CppExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Collects the lowercased simple names of every `class_specifier` and `struct_specifier`
/// declared anywhere in the translation unit (top level, inside namespaces, nested classes).
///
/// This first pass enables the local/external split when emitting `inherits` edges: a base whose
/// lowercased name appears in this set gets `base_id = "b_<name>"` (file-qualified), while an
/// external base gets `base_id = "<name>"` (bare lowercase name, matching graphify's out-of-file
/// reference convention).
fn local_class_names(root: &tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_class_names(root, source, &mut names);
    names
}

/// Recursively collects class/struct names from the named children of `node`.
///
/// Descends into `namespace_definition` bodies and nested `field_declaration_list` bodies so that
/// classes defined at any depth are captured for the local-type set.
fn collect_class_names(node: &tree_sitter::Node<'_>, source: &[u8], names: &mut HashSet<String>) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "class_specifier" | "struct_specifier" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if matches!(name_node.kind(), "type_identifier" | "identifier") {
                        names.insert(text_of(source, &name_node).to_lowercase());
                    }
                }
                // Recurse into the class body for nested class definitions.
                if let Some(body) = child.child_by_field_name("body") {
                    collect_class_names(&body, source, names);
                }
            }
            "namespace_definition" => {
                if let Some(body) = child.child_by_field_name("body") {
                    collect_class_names(&body, source, names);
                }
            }
            _ => {}
        }
    }
}

/// Drills through C++ declarator chains to extract the simple (lowercased) function or method
/// name.
///
/// The declarator chain for a function like `int* foo() {}` is:
/// `function_definition.declarator` → `pointer_declarator` → `function_declarator` →
/// `identifier("foo")`. This helper recurses through the chain, returning the leaf name.
///
/// Handled kinds:
/// - `identifier` / `field_identifier` → the leaf name (lowercased).
/// - `qualified_identifier` → its `name` field (the last component, e.g. `ns::Foo` → `"foo"`).
/// - `function_declarator` → its `declarator` field (recurse).
/// - `pointer_declarator` / `reference_declarator` → their `declarator` field (recurse).
///
/// Returns `None` for `destructor_name` (`~Foo`), `operator_name` (`operator+`),
/// `template_function`, and other unsupported forms — the caller silently skips those.
fn extract_fn_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(text_of(source, node).to_lowercase()),
        "qualified_identifier" => {
            // Take the unqualified last component: `ns::Foo` → `"foo"`.
            let name_node = node.child_by_field_name("name")?;
            if matches!(
                name_node.kind(),
                "identifier" | "field_identifier" | "type_identifier"
            ) {
                Some(text_of(source, &name_node).to_lowercase())
            } else {
                None
            }
        }
        "function_declarator" | "pointer_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            extract_fn_name(&inner, source)
        }
        "reference_declarator" => {
            // `reference_declarator` may not carry a named "declarator" field in all grammar
            // versions of tree-sitter-cpp. Try the named field first, then fall back to the
            // first named child (the `&`/`&&` token is anonymous and so is skipped).
            let inner = node
                .child_by_field_name("declarator")
                .or_else(|| node.named_child(0))?;
            extract_fn_name(&inner, source)
        }
        // destructor_name (~Foo), operator_name (operator+), template_function, etc. → None.
        _ => None,
    }
}

/// Extracts the lowercased base class names from a `base_class_clause` node.
///
/// Handles:
/// - `type_identifier` (plain name, e.g. `Base`).
/// - `qualified_identifier` (scoped name, e.g. `std::Base`, takes the last component).
/// - `base_specifier` wrappers present in some tree-sitter-cpp grammar versions.
///
/// Access specifiers (`public`, `private`, `protected`) and `virtual` are anonymous tokens in
/// the grammar and are therefore invisible to `named_child()`; they are silently skipped.
fn extract_base_names(node: &tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "type_identifier" => {
                names.push(text_of(source, &child).to_lowercase());
            }
            "qualified_identifier" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if matches!(
                        name_node.kind(),
                        "type_identifier" | "identifier" | "field_identifier"
                    ) {
                        names.push(text_of(source, &name_node).to_lowercase());
                    }
                }
            }
            // Some tree-sitter-cpp versions wrap each base in a `base_specifier` node.
            "base_specifier" => {
                for j in 0..child.named_child_count() {
                    let Some(inner) = child.named_child(j) else {
                        continue;
                    };
                    match inner.kind() {
                        "type_identifier" => {
                            names.push(text_of(source, &inner).to_lowercase());
                            break;
                        }
                        "qualified_identifier" => {
                            if let Some(name_node) = inner.child_by_field_name("name") {
                                if matches!(
                                    name_node.kind(),
                                    "type_identifier" | "identifier" | "field_identifier"
                                ) {
                                    names.push(text_of(source, &name_node).to_lowercase());
                                }
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Extracts a `function_definition` node, emitting a function node and a `contains` edge.
///
/// Emits a function node `B_<fn>` (lowercased) and a `contains` edge from `container` to the
/// node. Functions whose declarator chain cannot be resolved (destructors, operators, or
/// templates) are silently skipped.
fn extract_function(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    container: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(decl_node) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(fn_lower) = extract_fn_name(&decl_node, source) else {
        return;
    };
    let fn_label = format!("{b}_{fn_lower}");
    result.nodes.push(RawNode {
        label: fn_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: container.to_owned(),
        target: fn_label,
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Extracts a `class_specifier` or `struct_specifier` node into `result`.
///
/// Emits:
/// - A class/struct node `B_<name>` (lowercased class name) with a `contains` edge from
///   `container`.
/// - One `inherits` edge per base class in the `base_class_clause`. The `base_id` is
///   `"b_<base>"` for locally-declared bases (in `local_types`) and `"<base>"` (bare lowercase)
///   for external bases.
/// - One method node `B_<cls>_<method>` and one `method` edge per inline `function_definition`
///   in the class/struct body. Constructors (name == class name) and unresolvable names
///   (destructors, operators) are silently skipped.
fn extract_class(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    container: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if !matches!(name_node.kind(), "type_identifier" | "identifier") {
        return;
    }
    let c = text_of(source, &name_node).to_lowercase();
    let class_label = format!("{b}_{c}");

    result.nodes.push(RawNode {
        label: class_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: container.to_owned(),
        target: class_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // Walk named children for base_class_clause (inheritance) and field_declaration_list (body).
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "base_class_clause" => {
                let bases = extract_base_names(&child, source);
                for base_lower in bases {
                    let base_id = if local_types.contains(&base_lower) {
                        format!("{b}_{base_lower}")
                    } else {
                        base_lower
                    };
                    result.edges.push(RawEdge {
                        source: base_id,
                        target: class_label.clone(),
                        relation: "inherits".to_owned(),
                        confidence: Confidence::Extracted,
                    });
                }
            }
            "field_declaration_list" => {
                extract_methods(&child, source, b, &c, &class_label, source_file, result);
            }
            _ => {}
        }
    }
}

/// Extracts inline `function_definition` children from a `field_declaration_list` (class body).
///
/// Emits a method node `B_<cls>_<method>` (lowercased) and a `method` edge from `class_label`
/// to the method node for each qualifying `function_definition` child. Methods whose declarator
/// chain cannot be resolved (destructors, operators) and constructors (name matches class name)
/// are silently skipped.
fn extract_methods(
    body: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    c: &str,
    class_label: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for i in 0..body.named_child_count() {
        let Some(child) = body.named_child(i) else {
            continue;
        };
        if child.kind() != "function_definition" {
            continue;
        }
        let Some(decl_node) = child.child_by_field_name("declarator") else {
            continue;
        };
        let Some(method_lower) = extract_fn_name(&decl_node, source) else {
            // Destructors, operators → None; silently skip.
            continue;
        };
        // Skip constructor: method name == class name.
        if method_lower == c {
            continue;
        }
        let method_label = format!("{b}_{c}_{method_lower}");
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

/// Extracts a named `namespace_definition`, emitting a namespace node and recursing into its body.
///
/// Emits a namespace node `B_<ns>` (lowercased) and a `contains` edge from `container`.
/// Then walks each named child of the namespace body, passing the namespace node as the new
/// container so its contents receive `B_<ns>→item` containment edges. Anonymous namespaces
/// (no `name` field) are silently skipped.
fn extract_namespace(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    container: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) if matches!(n.kind(), "namespace_identifier" | "identifier") => n,
        _ => return, // anonymous namespace — skip
    };
    let ns_lower = text_of(source, &name_node).to_lowercase();
    let ns_label = format!("{b}_{ns_lower}");

    result.nodes.push(RawNode {
        label: ns_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: container.to_owned(),
        target: ns_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // Recurse into the namespace body, using the namespace node as the new container.
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };
    for i in 0..body.named_child_count() {
        let Some(child) = body.named_child(i) else {
            continue;
        };
        walk_node(&child, source, b, &ns_label, source_file, local_types, result);
    }
}

/// Processes a `preproc_include` node, emitting an `imports_from` edge from the file node `b`.
///
/// Handles `"header.h"` (`string_literal`, double-quoted) and `<header.h>` (`system_lib_string`,
/// angle-bracketed). The surrounding delimiters are stripped and the path is lowercased. Includes
/// always anchor to the file node `b`, not the current `container`, because `#include` directives
/// appear at translation-unit scope.
fn extract_include(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    result: &mut Extraction,
) {
    let Some(path_node) = node.child_by_field_name("path") else {
        return;
    };
    let raw = text_of(source, &path_node);
    // Strip surrounding " " or < >.
    let stripped = raw
        .trim_start_matches(['"', '<'])
        .trim_end_matches(['"', '>'])
        .to_lowercase();
    if stripped.is_empty() {
        return;
    }
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: stripped,
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Dispatches one AST node from a `translation_unit` or a namespace body to its extractor.
///
/// Recognised kinds:
/// - `function_definition` → [`extract_function`]
/// - `class_specifier` / `struct_specifier` → [`extract_class`]
/// - `namespace_definition` → [`extract_namespace`]
/// - `preproc_include` → [`extract_include`] (always from file node `b`)
///
/// All other kinds (`declaration`, `template_declaration`, `using_declaration`,
/// `typedef_declaration`, `linkage_specification`, `alias_declaration`, `static_assert`,
/// `preproc_def`, etc.) are silently ignored.
fn walk_node(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    container: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    match node.kind() {
        "function_definition" => {
            extract_function(node, source, b, container, source_file, result);
        }
        "class_specifier" | "struct_specifier" => {
            extract_class(node, source, b, container, source_file, local_types, result);
        }
        "namespace_definition" => {
            extract_namespace(node, source, b, container, source_file, local_types, result);
        }
        "preproc_include" => {
            // Include directives always anchor to the file node, regardless of current container.
            extract_include(node, source, b, result);
        }
        // All other node kinds: forward declarations, templates, using-directives, etc. → skip.
        _ => {}
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for CppExtractor {
    fn language(&self) -> &'static str {
        "cpp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cpp", "cc", "cxx", "hpp", "hxx"]
    }

    /// Extracts C++ nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// function nodes `B_<fn>`; class/struct nodes `B_<name>`; namespace nodes `B_<ns>`; method
    /// nodes `B_<cls>_<method>`; with `contains`, `method`, `inherits`, and `imports_from` edges.
    /// `calls` and `uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The C++ grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-cpp`).
    /// - `parser.parse` returns `None` (cancellation / timeout — not triggered by invalid C++
    ///   syntax; tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .map_err(|e| GraphError::Parse {
                file: source_file.clone(),
                message: e.to_string(),
            })?;

        let tree = parser.parse(source, None).ok_or_else(|| GraphError::Parse {
            file: source_file.clone(),
            message: "parse returned None".into(),
        })?;

        let root = tree.root_node();

        // Pass 1: collect locally-declared class/struct names for the local/external split.
        let local_types = local_class_names(&root, source);

        let mut result = Extraction::new();

        // Always emit the file node, even for an empty or garbled source file.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Pass 2: walk top-level translation_unit named children.
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            walk_node(&child, source, &b, &b, &source_file, &local_types, &mut result);
        }

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::CppExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on extractor error.
    fn extract(src: &str, filename: &str) -> Extraction {
        CppExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("cpp extractor failed on {filename}: {e}"))
    }

    /// Returns `true` if `ex` contains a node with the given label.
    fn has_node(ex: &Extraction, label: &str) -> bool {
        ex.nodes.iter().any(|n| n.label == label)
    }

    /// Returns `true` if `ex` contains an edge with the given source, target, and relation.
    fn has_edge(ex: &Extraction, src: &str, tgt: &str, rel: &str) -> bool {
        ex.edges
            .iter()
            .any(|e| e.source == src && e.target == tgt && e.relation == rel)
    }

    /// Returns the node with the given label; panics if absent.
    fn node<'e>(ex: &'e Extraction, label: &str) -> &'e habitat_graph_core::RawNode {
        ex.nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "node '{label}' not found; got {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    /// Returns all edges with the given relation.
    fn edges_with_rel<'e>(
        ex: &'e Extraction,
        rel: &str,
    ) -> Vec<&'e habitat_graph_core::RawEdge> {
        ex.edges.iter().filter(|e| e.relation == rel).collect()
    }

    // ── A. Empty input ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "empty.cpp");
        assert_eq!(ex.nodes.len(), 1, "empty: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
        assert_eq!(ex.edges.len(), 0, "empty: expected no edges");
    }

    #[test]
    fn whitespace_only_source_yields_file_node_only() {
        let ex = extract("   \n\n\t\n", "space.cpp");
        assert_eq!(ex.nodes.len(), 1, "whitespace: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "space");
        assert_eq!(ex.edges.len(), 0, "whitespace: expected no edges");
    }

    // ── B. File stem lowercasing ───────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "MyClass.cpp");
        assert_eq!(ex.nodes[0].label, "myclass");
    }

    #[test]
    fn file_stem_mixed_case_fully_lowercased() {
        let ex = extract("", "HTTPClient.cc");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_from_hpp_extension() {
        let ex = extract("", "Socket.hpp");
        assert_eq!(ex.nodes[0].label, "socket");
    }

    #[test]
    fn file_stem_from_cxx_extension() {
        let ex = extract("", "Parser.cxx");
        assert_eq!(ex.nodes[0].label, "parser");
    }

    #[test]
    fn file_stem_from_hxx_extension() {
        let ex = extract("", "Types.hxx");
        assert_eq!(ex.nodes[0].label, "types");
    }

    // ── C. Top-level functions ─────────────────────────────────────────────────────────────────

    #[test]
    fn function_emits_fn_node_and_contains_edge() {
        let src = "int add(int a, int b) { return a + b; }\n";
        let ex = extract(src, "math.cpp");
        assert!(has_node(&ex, "math"), "file node missing");
        assert!(has_node(&ex, "math_add"), "fn node missing");
        assert!(
            has_edge(&ex, "math", "math_add", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased() {
        let src = "void ParseXML() {}\n";
        let ex = extract(src, "parser.cpp");
        assert!(
            has_node(&ex, "parser_parsexml"),
            "function label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_functions_all_emitted() {
        let src = "void A() {}\nvoid B() {}\nvoid C() {}\n";
        let ex = extract(src, "funcs.cpp");
        assert!(has_node(&ex, "funcs_a"), "funcs_a missing");
        assert!(has_node(&ex, "funcs_b"), "funcs_b missing");
        assert!(has_node(&ex, "funcs_c"), "funcs_c missing");
        assert_eq!(
            ex.nodes.len(),
            4,
            "expected file node + 3 function nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn pointer_return_type_function_extracted() {
        let src = "int* allocate(int n) { return new int[n]; }\n";
        let ex = extract(src, "mem.cpp");
        assert!(
            has_node(&ex, "mem_allocate"),
            "pointer-return function must be extracted; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn function_with_reference_return_type_extracted() {
        let src = "int& get_ref(int& x) { return x; }\n";
        let ex = extract(src, "ref.cpp");
        assert!(
            has_node(&ex, "ref_get_ref"),
            "reference-return function must be extracted; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── D. Class extraction ────────────────────────────────────────────────────────────────────

    #[test]
    fn class_emits_node_and_contains_edge() {
        let src = "class Foo {};\n";
        let ex = extract(src, "foo.cpp");
        assert!(has_node(&ex, "foo"), "file node missing");
        assert!(has_node(&ex, "foo_foo"), "class node missing");
        assert!(
            has_edge(&ex, "foo", "foo_foo", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_lowercased() {
        let src = "class MyHTTPServer {};\n";
        let ex = extract(src, "server.cpp");
        assert!(
            has_node(&ex, "server_myhttpserver"),
            "class label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn struct_emits_node_and_contains_edge() {
        let src = "struct Point { int x; int y; };\n";
        let ex = extract(src, "geom.cpp");
        assert!(has_node(&ex, "geom_point"), "struct node missing");
        assert!(
            has_edge(&ex, "geom", "geom_point", "contains"),
            "struct contains edge missing"
        );
    }

    #[test]
    fn class_with_empty_body_emits_class_node_no_methods() {
        let src = "class Empty {};\n";
        let ex = extract(src, "empty.cpp");
        assert!(has_node(&ex, "empty_empty"), "class node missing");
        let method_edges = edges_with_rel(&ex, "method");
        assert_eq!(
            method_edges.len(),
            0,
            "empty class must have no method edges"
        );
    }

    #[test]
    fn multiple_classes_all_emitted() {
        let src = "class A {};\nclass B {};\nclass C {};\n";
        let ex = extract(src, "abc.cpp");
        assert!(has_node(&ex, "abc_a"), "abc_a missing");
        assert!(has_node(&ex, "abc_b"), "abc_b missing");
        assert!(has_node(&ex, "abc_c"), "abc_c missing");
    }

    // ── E. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn class_method_emits_method_node_and_edge() {
        let src = "class Dog {\n  void bark() {}\n};\n";
        let ex = extract(src, "dog.cpp");
        assert!(has_node(&ex, "dog_dog_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_dog", "dog_dog_bark", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn method_label_is_b_underscore_class_underscore_method() {
        let src = "class Cat {\n  void purr() {}\n};\n";
        let ex = extract(src, "cat.cpp");
        let label = "cat_cat_purr";
        assert!(
            has_node(&ex, label),
            "expected label '{label}'; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_methods_all_emitted() {
        let src = "class Calc {\n  int add() {return 0;}\n  int sub() {return 0;}\n  int mul() {return 0;}\n};\n";
        let ex = extract(src, "calc.cpp");
        assert!(has_node(&ex, "calc_calc_add"), "calc_calc_add missing");
        assert!(has_node(&ex, "calc_calc_sub"), "calc_calc_sub missing");
        assert!(has_node(&ex, "calc_calc_mul"), "calc_calc_mul missing");
        let method_edges = edges_with_rel(&ex, "method");
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn constructor_is_skipped_not_emitted_as_method() {
        // Constructor name matches class name → must be skipped.
        let src = "class Timer {\n  Timer() {}\n  void start() {}\n};\n";
        let ex = extract(src, "timer.cpp");
        // Timer() constructor → should NOT produce timer_timer_timer node.
        assert!(
            !has_node(&ex, "timer_timer_timer"),
            "constructor must not be emitted as method"
        );
        // start() method → must be present.
        assert!(
            has_node(&ex, "timer_timer_start"),
            "regular method 'start' must be present"
        );
    }

    #[test]
    fn struct_with_methods_emits_method_nodes() {
        let src = "struct Vec3 {\n  float length() { return 0.0f; }\n};\n";
        let ex = extract(src, "vec.cpp");
        assert!(has_node(&ex, "vec_vec3_length"), "struct method missing");
        assert!(
            has_edge(&ex, "vec_vec3", "vec_vec3_length", "method"),
            "struct method edge missing"
        );
    }

    // ── F. Inheritance ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn local_class_inherits_emits_inherits_edge() {
        let src = "class Base {};\nclass Derived : public Base {};\n";
        let ex = extract(src, "hier.cpp");
        // Both Base and Derived are local → base_id = "hier_base"
        assert!(
            has_edge(&ex, "hier_base", "hier_derived", "inherits"),
            "local inherits edge missing; edges = {:?}",
            ex.edges
                .iter()
                .map(|e| (&e.source, &e.target, &e.relation))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn external_class_inherits_uses_bare_base_id() {
        let src = "class Derived : public ExternalBase {};\n";
        let ex = extract(src, "derived.cpp");
        // ExternalBase not in file → base_id = "externalbase"
        assert!(
            has_edge(&ex, "externalbase", "derived_derived", "inherits"),
            "external inherits edge missing; edges = {:?}",
            ex.edges
                .iter()
                .map(|e| (&e.source, &e.target, &e.relation))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn local_base_has_file_qualified_id() {
        let src = "class Animal {};\nclass Dog : public Animal {};\n";
        let ex = extract(src, "pets.cpp");
        let inherits = edges_with_rel(&ex, "inherits");
        assert_eq!(inherits.len(), 1, "expected exactly 1 inherits edge");
        assert_eq!(
            inherits[0].source, "pets_animal",
            "local base must be file-qualified"
        );
    }

    #[test]
    fn external_base_has_bare_lowercase_id() {
        let src = "class Widget : public QWidget {};\n";
        let ex = extract(src, "ui.cpp");
        let inherits = edges_with_rel(&ex, "inherits");
        assert_eq!(inherits.len(), 1, "expected exactly 1 inherits edge");
        assert_eq!(
            inherits[0].source, "qwidget",
            "external base must be bare lowercase"
        );
    }

    #[test]
    fn multiple_inheritance_emits_multiple_inherits_edges() {
        let src = "class A {};\nclass B {};\nclass C : public A, public B {};\n";
        let ex = extract(src, "multi.cpp");
        let inherits = edges_with_rel(&ex, "inherits");
        assert_eq!(
            inherits.len(),
            2,
            "expected 2 inherits edges for double inheritance; got {:?}",
            inherits
                .iter()
                .map(|e| (&e.source, &e.target))
                .collect::<Vec<_>>()
        );
        assert!(
            has_edge(&ex, "multi_a", "multi_c", "inherits"),
            "A→C inherits missing"
        );
        assert!(
            has_edge(&ex, "multi_b", "multi_c", "inherits"),
            "B→C inherits missing"
        );
    }

    // ── G. Namespaces ──────────────────────────────────────────────────────────────────────────

    #[test]
    fn namespace_emits_node_and_file_contains_edge() {
        let src = "namespace net {}\n";
        let ex = extract(src, "net.cpp");
        assert!(has_node(&ex, "net_net"), "namespace node missing");
        assert!(
            has_edge(&ex, "net", "net_net", "contains"),
            "file→ns contains missing"
        );
    }

    #[test]
    fn namespace_name_is_lowercased() {
        let src = "namespace HTTP {}\n";
        let ex = extract(src, "proto.cpp");
        assert!(
            has_node(&ex, "proto_http"),
            "namespace label must be lowercased"
        );
    }

    #[test]
    fn function_inside_namespace_has_ns_contains_edge() {
        let src = "namespace net {\n  void connect() {}\n}\n";
        let ex = extract(src, "net.cpp");
        // Function should be labeled b_<fn> = net_connect
        assert!(has_node(&ex, "net_connect"), "function node missing");
        // Container is the namespace node net_net
        assert!(
            has_edge(&ex, "net_net", "net_connect", "contains"),
            "ns→fn contains missing"
        );
    }

    #[test]
    fn class_inside_namespace_has_ns_contains_edge() {
        let src = "namespace core {\n  class Buffer {};\n}\n";
        let ex = extract(src, "core.cpp");
        assert!(has_node(&ex, "core_buffer"), "class in ns missing");
        assert!(
            has_edge(&ex, "core_core", "core_buffer", "contains"),
            "ns→class contains missing"
        );
    }

    #[test]
    fn anonymous_namespace_is_skipped() {
        let src = "namespace {\n  void helper() {}\n}\n";
        let ex = extract(src, "anon.cpp");
        // Anonymous namespace has no stable name → skip the namespace node itself.
        // The helper function may or may not be emitted depending on tree shape, but
        // the key invariant is: no namespace node with an anonymous label is emitted.
        let ns_nodes: Vec<_> = ex
            .nodes
            .iter()
            .filter(|n| n.label != "anon")
            .filter(|n| {
                let l = &n.label;
                // A namespace-like node would have the same prefix as the file.
                // Since the namespace is anonymous, there's no valid node for it.
                !l.ends_with("_helper") // allow function nodes if emitted
            })
            .collect();
        // The only non-helper, non-file node would be a (wrongly-emitted) anon namespace node.
        // We just check that no contains edge originates from an empty-named or "anon_" prefix
        // that represents the anon namespace itself — i.e., there's no node for the ns.
        let has_anon_ns = ex.nodes.iter().any(|n| {
            n.label.starts_with("anon_") && !n.label.ends_with("_helper")
        });
        assert!(!has_anon_ns || ns_nodes.is_empty(), "anonymous namespace must not emit a node");
    }

    // ── H. #include → imports_from ─────────────────────────────────────────────────────────────

    #[test]
    fn system_include_emits_imports_from_edge() {
        let src = "#include <vector>\n";
        let ex = extract(src, "main.cpp");
        assert!(
            has_edge(&ex, "main", "vector", "imports_from"),
            "system include must emit imports_from; edges={:?}",
            ex.edges
                .iter()
                .map(|e| (&e.source, &e.target, &e.relation))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn local_include_emits_imports_from_edge() {
        let src = "#include \"mylib.h\"\n";
        let ex = extract(src, "main.cpp");
        assert!(
            has_edge(&ex, "main", "mylib.h", "imports_from"),
            "local include must emit imports_from"
        );
    }

    #[test]
    fn include_path_is_lowercased() {
        let src = "#include <MyLib/Utils.h>\n";
        let ex = extract(src, "main.cpp");
        assert!(
            has_edge(&ex, "main", "mylib/utils.h", "imports_from"),
            "include path must be lowercased"
        );
    }

    #[test]
    fn include_anchors_to_file_node_not_namespace() {
        // Even inside (conceptually, though #include is always at TU scope) a namespace block,
        // the include should anchor to the file node.
        let src = "#include <cstdio>\nnamespace io {}\n";
        let ex = extract(src, "io.cpp");
        // include→cstdio must come from file node "io", not from "io_io" (namespace).
        assert!(
            has_edge(&ex, "io", "cstdio", "imports_from"),
            "include must anchor to file node"
        );
        assert!(
            !has_edge(&ex, "io_io", "cstdio", "imports_from"),
            "include must NOT anchor to namespace node"
        );
    }

    // ── I. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = "void foo() { bar(); }\nvoid bar() {}\n";
        let ex = extract(src, "calls.cpp");
        let calls = edges_with_rel(&ex, "calls");
        assert!(
            calls.is_empty(),
            "calls edges must never be emitted; got {calls:?}"
        );
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "int x = 5;\nvoid foo() { int y = x; }\n";
        let ex = extract(src, "uses.cpp");
        let uses = edges_with_rel(&ex, "uses");
        assert!(
            uses.is_empty(),
            "uses edges must never be emitted; got {uses:?}"
        );
    }

    #[test]
    fn standalone_functions_have_no_inherits_edges() {
        let src = "void a() {}\nvoid b() {}\n";
        let ex = extract(src, "funcs.cpp");
        let inh = edges_with_rel(&ex, "inherits");
        assert!(
            inh.is_empty(),
            "plain functions must not produce inherits edges"
        );
    }

    #[test]
    fn destructor_is_not_emitted_as_method() {
        let src = "class Foo {\n  ~Foo() {}\n  void work() {}\n};\n";
        let ex = extract(src, "dtor.cpp");
        // ~Foo is a destructor → must NOT appear as a method node.
        assert!(
            !has_node(&ex, "dtor_foo_~foo"),
            "destructor must not be emitted"
        );
        // The regular method must still appear.
        assert!(
            has_node(&ex, "dtor_foo_work"),
            "regular method 'work' must be present after destructor skip"
        );
    }

    #[test]
    fn no_inherits_for_structs_without_bases() {
        let src = "struct Plain { int x; };\n";
        let ex = extract(src, "s.cpp");
        let inh = edges_with_rel(&ex, "inherits");
        assert!(
            inh.is_empty(),
            "struct without base class must not emit inherits"
        );
    }

    // ── J. Node/edge properties ────────────────────────────────────────────────────────────────

    #[test]
    fn all_nodes_have_source_file_set() {
        let src = "class Foo {};\nvoid bar() {}\n";
        let ex = extract(src, "props.cpp");
        for n in &ex.nodes {
            assert_eq!(
                n.source_file, "props.cpp",
                "node '{}' must have source_file='props.cpp'",
                n.label
            );
        }
    }

    #[test]
    fn all_edges_have_confidence_extracted() {
        let src = "class Base {};\nclass Derived : public Base {\n  void act() {}\n};\n";
        let ex = extract(src, "conf.cpp");
        for e in &ex.edges {
            assert_eq!(
                e.confidence,
                Confidence::Extracted,
                "edge {}→{} ({}) must have Confidence::Extracted",
                e.source,
                e.target,
                e.relation
            );
        }
    }

    #[test]
    fn file_node_has_non_zero_span() {
        let src = "class Foo {};\n";
        let ex = extract(src, "span.cpp");
        let n = node(&ex, "span");
        // A non-empty source → end_byte must be > 0.
        assert!(
            n.span.end_byte > 0,
            "file node span must cover the source bytes"
        );
    }

    #[test]
    fn symbol_node_has_span_with_nonzero_start_line() {
        // The function is on line 2 (after the comment).
        let src = "// comment\nvoid go() {}\n";
        let ex = extract(src, "line.cpp");
        let n = node(&ex, "line_go");
        assert!(
            n.span.start_line >= 1,
            "function start_line must be >= 1; got {n:?}"
        );
    }

    #[test]
    fn method_edge_has_confidence_extracted() {
        let src = "class C {\n  void m() {}\n};\n";
        let ex = extract(src, "me.cpp");
        let method_edges = edges_with_rel(&ex, "method");
        assert_eq!(method_edges.len(), 1);
        assert_eq!(method_edges[0].confidence, Confidence::Extracted);
    }

    #[test]
    fn inherits_edge_has_confidence_extracted() {
        let src = "class Base {};\nclass Derived : public Base {};\n";
        let ex = extract(src, "inh.cpp");
        let inh = edges_with_rel(&ex, "inherits");
        assert_eq!(inh.len(), 1);
        assert_eq!(inh[0].confidence, Confidence::Extracted);
    }

    // ── K. Qualified-id format ─────────────────────────────────────────────────────────────────

    #[test]
    fn class_label_format_is_b_classname() {
        let src = "class Widget {};\n";
        let ex = extract(src, "ui.cpp");
        // File stem = "ui", class = "widget" → label = "ui_widget"
        assert!(has_node(&ex, "ui_widget"), "class label must be b_classname");
    }

    #[test]
    fn function_label_format_is_b_fnname() {
        let src = "void render() {}\n";
        let ex = extract(src, "gfx.cpp");
        assert!(has_node(&ex, "gfx_render"), "fn label must be b_fnname");
    }

    #[test]
    fn method_label_format_is_b_class_method() {
        let src = "class Engine {\n  void run() {}\n};\n";
        let ex = extract(src, "eng.cpp");
        assert!(
            has_node(&ex, "eng_engine_run"),
            "method label must be b_class_method"
        );
    }

    // ── L. Language / extensions ───────────────────────────────────────────────────────────────

    #[test]
    fn language_returns_cpp() {
        assert_eq!(CppExtractor.language(), "cpp");
    }

    #[test]
    fn extensions_contain_cpp() {
        assert!(CppExtractor.extensions().contains(&"cpp"));
    }

    #[test]
    fn extensions_contain_cc() {
        assert!(CppExtractor.extensions().contains(&"cc"));
    }

    #[test]
    fn extensions_contain_cxx() {
        assert!(CppExtractor.extensions().contains(&"cxx"));
    }

    #[test]
    fn extensions_contain_hpp() {
        assert!(CppExtractor.extensions().contains(&"hpp"));
    }

    #[test]
    fn extensions_contain_hxx() {
        assert!(CppExtractor.extensions().contains(&"hxx"));
    }

    // ── M. Misc / edge cases ───────────────────────────────────────────────────────────────────

    #[test]
    fn syntax_error_input_does_not_panic() {
        // tree-sitter is error-tolerant; partial/broken input must not cause a panic or Err.
        let src = "class { broken syntax !!! @#$% void(( }\n";
        let result = CppExtractor.extract(Path::new("broken.cpp"), src.as_bytes());
        // Must succeed (tree-sitter produces a partial tree) — the result may be empty of
        // symbols but must not be Err.
        assert!(
            result.is_ok(),
            "syntax error input must not return Err; got {result:?}"
        );
    }

    #[test]
    fn unicode_in_string_literal_does_not_panic() {
        let src = "const char* msg = \"こんにちは world 🦀\";\n";
        let result = CppExtractor.extract(Path::new("unicode.cpp"), src.as_bytes());
        assert!(result.is_ok(), "unicode input must parse without error");
    }

    #[test]
    fn include_and_class_in_same_file() {
        let src = "#include <string>\nclass Greeter {\n  void greet() {}\n};\n";
        let ex = extract(src, "greeter.cpp");
        assert!(has_node(&ex, "greeter_greeter"), "class node missing");
        assert!(
            has_edge(&ex, "greeter", "string", "imports_from"),
            "imports_from edge missing"
        );
        assert!(
            has_edge(&ex, "greeter_greeter", "greeter_greeter_greet", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn composite_realistic_header_file() {
        let src = r#"
#include <cstdint>
#include "base.hpp"

namespace engine {

class Component {
public:
    virtual void update() {}
    virtual void render() {}
};

class Transform : public Component {
public:
    void update() {}
    void set_position(float x, float y) {}
};

}  // namespace engine
"#;
        let ex = extract(src, "component.hpp");

        // File node present.
        assert!(has_node(&ex, "component"), "file node missing");

        // Includes.
        assert!(
            has_edge(&ex, "component", "cstdint", "imports_from"),
            "cstdint import missing"
        );
        assert!(
            has_edge(&ex, "component", "base.hpp", "imports_from"),
            "base.hpp import missing"
        );

        // Namespace.
        assert!(has_node(&ex, "component_engine"), "namespace node missing");
        assert!(
            has_edge(&ex, "component", "component_engine", "contains"),
            "file→ns contains missing"
        );

        // Component class.
        assert!(has_node(&ex, "component_component"), "Component class missing");
        assert!(
            has_edge(&ex, "component_engine", "component_component", "contains"),
            "ns→Component contains missing"
        );

        // Transform class.
        assert!(has_node(&ex, "component_transform"), "Transform class missing");

        // Transform inherits Component (local).
        assert!(
            has_edge(
                &ex,
                "component_component",
                "component_transform",
                "inherits"
            ),
            "Transform inherits Component missing"
        );

        // Methods on Transform.
        assert!(
            has_node(&ex, "component_transform_update"),
            "transform update method missing"
        );
        assert!(
            has_node(&ex, "component_transform_set_position"),
            "transform set_position method missing"
        );
    }

    #[test]
    fn nested_namespace_carries_through() {
        let src = "namespace outer {\n  namespace inner {\n    void helper() {}\n  }\n}\n";
        let ex = extract(src, "nested.cpp");
        // outer namespace.
        assert!(has_node(&ex, "nested_outer"), "outer ns missing");
        assert!(
            has_edge(&ex, "nested", "nested_outer", "contains"),
            "file→outer missing"
        );
        // inner namespace inside outer.
        assert!(has_node(&ex, "nested_inner"), "inner ns missing");
        assert!(
            has_edge(&ex, "nested_outer", "nested_inner", "contains"),
            "outer→inner contains missing"
        );
        // helper inside inner.
        assert!(has_node(&ex, "nested_helper"), "helper fn missing");
        assert!(
            has_edge(&ex, "nested_inner", "nested_helper", "contains"),
            "inner→helper contains missing"
        );
    }

    #[test]
    fn struct_inheritance_emits_inherits_edge() {
        let src = "struct Base { int x; };\nstruct Derived : public Base { int y; };\n";
        let ex = extract(src, "structs.cpp");
        assert!(
            has_edge(&ex, "structs_base", "structs_derived", "inherits"),
            "struct inheritance missing; edges={:?}",
            ex.edges
                .iter()
                .map(|e| (&e.source, &e.target, &e.relation))
                .collect::<Vec<_>>()
        );
    }
}
