//! Java AST extractor (`tree-sitter-java`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class nodes
//! `B_<name>`; interface nodes `B_<name>`; enum nodes `B_<name>`; method and constructor nodes
//! `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class→method), `inherits`
//! (extends and implements, with local/external split), `imports_from` (file→import path).
//! `calls` and `uses` are deliberately **NOT** emitted — they are name-resolution heuristics
//! that would not match graphify.
//!
//! Abstract classes use the same `class_declaration` node kind in tree-sitter-java and are
//! handled identically to concrete classes. Wildcard imports (`java.util.*`) emit the package
//! path without the trailing `.*`.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from Java source using `tree-sitter-java`, in graphify's
/// qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (lowercased stem): one file node `B`; class, interface, and
/// enum nodes `B_<name>`; method and constructor nodes `B_<cls>_<method>`. Edges: `contains`
/// (file→symbol), `method` (class→method), `inherits` (base→derived, with local/external
/// split for both `extends` and `implements`), `imports_from` (file→import path).
/// `calls`/`uses` are deliberately omitted.
///
/// An empty file produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct JavaExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Returns the method id component `m` of the qualified label `B_C_m`.
///
/// Strips leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting only
/// of underscores).
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Collects the lowercased names of all top-level class, interface, and enum declarations.
///
/// Used in the first pass to distinguish local bases (same file) from external ones when
/// emitting `inherits` edges: a local base `Foo` in file with stem `b` gets
/// `base_id = "b_foo"`, while an external base gets `base_id = "foo"`.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        if matches!(
            child.kind(),
            "class_declaration" | "interface_declaration" | "enum_declaration"
        ) {
            if let Some(name_node) = child.child_by_field_name("name") {
                names.insert(text_of(source, &name_node).to_lowercase());
            }
        }
    }
    names
}

/// Resolves a type node to its lowercased simple name.
///
/// Handles `type_identifier` (plain) and `generic_type` (parameterised, e.g. `List<String>`,
/// where the base identifier `List` is extracted). Returns `None` for scoped/qualified types
/// (`scoped_type_identifier`) and any other node kind — those are silently skipped.
fn resolve_type_identifier(type_node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match type_node.kind() {
        "type_identifier" => Some(text_of(source, type_node).to_lowercase()),
        "generic_type" => {
            // First named child of a generic_type is the raw type identifier.
            let inner = type_node.named_child(0)?;
            if inner.kind() == "type_identifier" {
                Some(text_of(source, &inner).to_lowercase())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Emits an `inherits` edge applying the local/external split.
fn emit_inherits_from_name(
    base_lower: &str,
    b: &str,
    target_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let base_id = if local_types.contains(base_lower) {
        format!("{b}_{base_lower}")
    } else {
        base_lower.to_owned()
    };
    result.edges.push(RawEdge {
        source: base_id,
        target: target_label.to_owned(),
        relation: "inherits".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Recursively collects `inherits` edges from a type-list container node.
///
/// Handles `interface_type_list`, `super_interfaces`, `extends_interfaces`, and nested
/// `generic_type` nodes — recursing into any unrecognised container to find
/// `type_identifier` leaves.
fn collect_inherits_from_type_list(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    target_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if let Some(base_lower) = resolve_type_identifier(&child, source) {
            emit_inherits_from_name(&base_lower, b, target_label, local_types, result);
        } else {
            // Recurse into container nodes (interface_type_list, super_interfaces, etc.)
            collect_inherits_from_type_list(&child, source, b, target_label, local_types, result);
        }
    }
}

/// Extracts one `import_declaration` into `result`.
///
/// Emits an `imports_from` edge from `B` to the lowercased, dot-separated import path.
/// Wildcard imports (`import java.util.*;`) emit the package prefix only (`"java.util"`).
/// Static imports emit the full qualified member path.
///
/// # Errors
///
/// Never returns an error; import parsing failures are silently skipped.
fn extract_import(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    result: &mut Extraction,
) {
    // The first `identifier` or `scoped_identifier` named child is the import path.
    // The `static` keyword and `.*` are anonymous nodes and are not named children.
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "identifier" | "scoped_identifier") {
            let raw = text_of(source, &child);
            let module = raw.to_lowercase();
            if !module.is_empty() {
                result.edges.push(RawEdge {
                    source: b.to_owned(),
                    target: module,
                    relation: "imports_from".to_owned(),
                    confidence: Confidence::Extracted,
                });
            }
            return;
        }
    }
}

/// Extracts a `class_declaration` (concrete or abstract) into `result`.
///
/// Emits:
/// - A class node `B_c` and a `contains` edge from `B`.
/// - One `inherits` edge per resolved base in the `superclass` (extends) field.
/// - One `inherits` edge per resolved type in the `interfaces` (implements) field.
/// - One method/constructor node `B_c_m` and a `method` edge per `method_declaration` or
///   `constructor_declaration` in the class body.
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

    // Superclass: `child_by_field_name("superclass")` returns the `superclass` node
    // (`extends <type>`); walk its named children for a resolvable type identifier.
    if let Some(superclass_node) = node.child_by_field_name("superclass") {
        for si in 0..superclass_node.named_child_count() {
            let Some(type_node) = superclass_node.named_child(si) else {
                continue;
            };
            if let Some(base_lower) = resolve_type_identifier(&type_node, source) {
                emit_inherits_from_name(&base_lower, b, &class_label, local_types, result);
                break; // Java has exactly one superclass
            }
        }
    }

    // Interfaces: `child_by_field_name("interfaces")` returns the `super_interfaces` node
    // (`implements <type_list>`); recurse to collect all type identifiers.
    if let Some(ifaces_node) = node.child_by_field_name("interfaces") {
        collect_inherits_from_type_list(&ifaces_node, source, b, &class_label, local_types, result);
    }

    // Class body: extract method_declaration and constructor_declaration children.
    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(&body, source, b, &c, &class_label, source_file, result);
    }
}

/// Extracts `method_declaration` and `constructor_declaration` children of a class-like body.
///
/// Emits a method node `B_c_m` and a `method` edge from `class_label` for each qualifying
/// child. The `method_id` helper strips leading/trailing `_` from the name before lowercasing.
fn extract_body_methods(
    body: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    c: &str,
    class_label: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for idx in 0..body.named_child_count() {
        let Some(child) = body.named_child(idx) else {
            continue;
        };
        if !matches!(child.kind(), "method_declaration" | "constructor_declaration") {
            continue;
        }
        let Some(mname_node) = child.child_by_field_name("name") else {
            continue;
        };
        let raw_method = text_of(source, &mname_node);
        let m = method_id(&raw_method);
        let method_label = format!("{b}_{c}_{m}");
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

/// Extracts one `interface_declaration` into `result`.
///
/// Emits an interface node `B_<iface>` and a `contains` edge from `B`. If the interface
/// declares `extends` clauses, one `inherits` edge per resolved parent interface is emitted.
/// Interface member signatures are not individually emitted (the taxonomy treats interfaces
/// as monolithic symbols).
fn extract_interface(
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
    let iface_lower = text_of(source, &name_node).to_lowercase();
    let iface_label = format!("{b}_{iface_lower}");

    result.nodes.push(RawNode {
        label: iface_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: iface_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // In tree-sitter-java 0.23.5, `extends_interfaces` is an unkeyed named child
    // (not a field), so we walk by kind rather than using child_by_field_name.
    for ei in 0..node.named_child_count() {
        let Some(child) = node.named_child(ei) else {
            continue;
        };
        if child.kind() == "extends_interfaces" {
            collect_inherits_from_type_list(&child, source, b, &iface_label, local_types, result);
        }
    }
}

/// Extracts one `enum_declaration` into `result`.
///
/// Emits an enum node `B_<enum>` and a `contains` edge from `B`. If the enum declares
/// `implements` clauses, one `inherits` edge per resolved type is emitted. Methods declared
/// in the enum body's `enum_body_declarations` section are extracted as method nodes.
fn extract_enum(
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
    let enum_lower = text_of(source, &name_node).to_lowercase();
    let enum_label = format!("{b}_{enum_lower}");

    result.nodes.push(RawNode {
        label: enum_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: enum_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // Implements clause (`implements <type_list>`)
    if let Some(ifaces_node) = node.child_by_field_name("interfaces") {
        collect_inherits_from_type_list(
            &ifaces_node,
            source,
            b,
            &enum_label,
            local_types,
            result,
        );
    }

    // Methods inside enum_body_declarations (the section after the `;` in the enum body)
    if let Some(body) = node.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else {
                continue;
            };
            if child.kind() == "enum_body_declarations" {
                extract_body_methods(
                    &child,
                    source,
                    b,
                    &enum_lower,
                    &enum_label,
                    source_file,
                    result,
                );
            }
        }
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for JavaExtractor {
    fn language(&self) -> &'static str {
        "java"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    /// Extracts Java nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// class, interface, and enum nodes `B_<name>`; method and constructor nodes
    /// `B_<cls>_<method>`; with `contains`, `method`, `inherits`, and `imports_from` edges.
    /// `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Java grammar could not be installed on the parser (should not occur with a
    ///   correctly linked `tree-sitter-java`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid Java syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_java::LANGUAGE.into())
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

        // Pass 1: collect locally-declared type names for the local/external inherits split.
        let local_types = local_type_names(root, source);

        let mut result = Extraction::new();

        // Always emit the file node, even for empty or garbled source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Pass 2: walk top-level program children.
        for idx in 0..root.named_child_count() {
            let Some(child) = root.named_child(idx) else {
                continue;
            };
            match child.kind() {
                "import_declaration" => {
                    extract_import(&child, source, &b, &mut result);
                }
                "class_declaration" => {
                    extract_class(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_types,
                        &mut result,
                    );
                }
                "interface_declaration" => {
                    extract_interface(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_types,
                        &mut result,
                    );
                }
                "enum_declaration" => {
                    extract_enum(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_types,
                        &mut result,
                    );
                }
                // package_declaration, annotation_type_declaration, module_declaration — skipped.
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

    use super::JavaExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        JavaExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("java extractor failed on {filename}: {e}"))
    }

    /// Returns `true` if `ex` contains a node with the given label.
    fn has_node(ex: &Extraction, label: &str) -> bool {
        ex.nodes.iter().any(|n| n.label == label)
    }

    /// Returns `true` if `ex` contains an edge matching source, target, and relation.
    fn has_edge(ex: &Extraction, src: &str, tgt: &str, rel: &str) -> bool {
        ex.edges
            .iter()
            .any(|e| e.source == src && e.target == tgt && e.relation == rel)
    }

    /// Returns the node with the given label, panicking with a diagnostic if absent.
    fn get_node<'e>(ex: &'e Extraction, label: &str) -> &'e habitat_graph_core::RawNode {
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

    // ── A. Empty / trivial ─────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "Foo.java");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "foo");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn package_only_source_yields_only_file_node() {
        let ex = extract("package com.example;", "App.java");
        assert_eq!(ex.nodes.len(), 1, "package-only: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "app");
        assert_eq!(ex.edges.len(), 0, "package-only: expected no edges");
    }

    #[test]
    fn malformed_source_returns_ok_tree_sitter_is_error_tolerant() {
        let ex = extract("!!! NOT VALID JAVA @@@", "broken.java");
        assert!(has_node(&ex, "broken"), "file node must be present for garbled source");
    }

    // ── B. File stem handling ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HTTPClient.java");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_all_caps_fully_lowercased() {
        let ex = extract("", "APIUTILS.java");
        assert_eq!(ex.nodes[0].label, "apiutils");
    }

    #[test]
    fn file_stem_preserves_underscores_and_digits() {
        let ex = extract("", "my_module2.java");
        assert_eq!(ex.nodes[0].label, "my_module2");
    }

    #[test]
    fn language_slug_is_java() {
        assert_eq!(JavaExtractor.language(), "java");
    }

    #[test]
    fn extension_is_java() {
        assert_eq!(JavaExtractor.extensions(), &["java"]);
    }

    // ── C. Single class ────────────────────────────────────────────────────────────────────────

    #[test]
    fn single_class_emits_class_node_and_contains_edge() {
        let ex = extract("class Foo {}", "Foo.java");
        assert!(has_node(&ex, "foo_foo"), "class node missing");
        assert!(
            has_edge(&ex, "foo", "foo_foo", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_fully_lowercased_in_label() {
        let ex = extract("class HTTPParser {}", "parser.java");
        assert!(
            has_node(&ex, "parser_httpparser"),
            "class label must be fully lowercased"
        );
    }

    #[test]
    fn qualified_id_format_is_b_underscore_classname() {
        let ex = extract("class MyClass {}", "mymod.java");
        assert!(
            has_node(&ex, "mymod_myclass"),
            "class label must match B_ClassName pattern"
        );
    }

    // ── D. Multiple classes ────────────────────────────────────────────────────────────────────

    #[test]
    fn multiple_classes_all_emitted() {
        let src = "class A {} class B {} class C {}";
        let ex = extract(src, "multi.java");
        assert!(has_node(&ex, "multi_a"), "A missing");
        assert!(has_node(&ex, "multi_b"), "B missing");
        assert!(has_node(&ex, "multi_c"), "C missing");
    }

    #[test]
    fn public_and_package_private_classes_both_emitted() {
        let src = "public class Pub {} class Pkg {}";
        let ex = extract(src, "classes.java");
        assert!(has_node(&ex, "classes_pub"), "public class missing");
        assert!(has_node(&ex, "classes_pkg"), "package-private class missing");
    }

    // ── E. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_emits_method_node_and_method_edge() {
        let src = "class Dog { void bark() {} }";
        let ex = extract(src, "dog.java");
        assert!(has_node(&ex, "dog_dog_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_dog", "dog_dog_bark", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn method_name_is_lowercased_in_label() {
        let src = "class Foo { void myMethod() {} }";
        let ex = extract(src, "m.java");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method label must be fully lowercased"
        );
    }

    #[test]
    fn method_qualified_id_format_is_b_cls_method() {
        let src = "class MyClass { void doWork() {} }";
        let ex = extract(src, "mymod.java");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method label must match B_ClassName_methodName"
        );
    }

    #[test]
    fn multiple_methods_in_class_all_emitted() {
        let src = "class Foo { void alpha() {} int beta() { return 0; } String gamma() { return null; } }";
        let ex = extract(src, "methods.java");
        assert!(has_node(&ex, "methods_foo_alpha"), "alpha missing");
        assert!(has_node(&ex, "methods_foo_beta"), "beta missing");
        assert!(has_node(&ex, "methods_foo_gamma"), "gamma missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let src = "class Dog { void run() {} }";
        let ex = extract(src, "pet.java");
        assert!(has_edge(&ex, "pet_dog", "pet_dog_run", "method"));
        assert!(
            !has_edge(&ex, "pet", "pet_dog_run", "method"),
            "method edge source must be class label, not file label"
        );
    }

    #[test]
    fn method_nodes_do_not_appear_via_contains_edge() {
        let src = "class Foo { void go() {} }";
        let ex = extract(src, "f.java");
        let method_via_contains = ex
            .edges
            .iter()
            .any(|e| e.relation == "contains" && e.target == "f_foo_go");
        assert!(
            !method_via_contains,
            "method node must NOT appear via a contains edge"
        );
        assert!(has_edge(&ex, "f_foo", "f_foo_go", "method"));
    }

    // ── F. Constructor extraction ──────────────────────────────────────────────────────────────

    #[test]
    fn constructor_is_extracted_as_method_node() {
        let src = "class Widget { Widget() {} }";
        let ex = extract(src, "w.java");
        assert!(
            has_node(&ex, "w_widget_widget"),
            "constructor node must be extracted"
        );
        assert!(has_edge(&ex, "w_widget", "w_widget_widget", "method"));
    }

    #[test]
    fn constructor_with_params_is_extracted() {
        let src = "class Point { Point(int x, int y) {} }";
        let ex = extract(src, "p.java");
        assert!(has_node(&ex, "p_point_point"), "constructor with params node missing");
    }

    // ── G. Heritage — extends ─────────────────────────────────────────────────────────────────

    #[test]
    fn extends_external_class_emits_inherits_edge_with_bare_base_id() {
        let src = "class Dog extends Animal {}";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external extends must yield bare lowercased base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_local_class_emits_inherits_edge_with_stem_prefixed_base_id() {
        let src = "class A {} class B extends A {}";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local extends must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_with_no_heritage_has_no_inherits_edges() {
        let src = "class Standalone {}";
        let ex = extract(src, "s.java");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "class with no heritage must not produce inherits edges"
        );
    }

    #[test]
    fn generic_extends_plain_type_emits_inherits_edge() {
        // class Stack<T> extends java.util.ArrayList<T> → qualified → skipped (not type_identifier)
        // but class Stack<T> extends AbstractList<T> → generic_type → base identifier extracted
        let src = "class Stack<T> extends AbstractList<T> {}";
        let ex = extract(src, "s.java");
        assert!(
            has_edge(&ex, "abstractlist", "s_stack", "inherits"),
            "generic extends with plain type_identifier must emit inherits; edges: {:?}",
            ex.edges
        );
    }

    // ── H. Heritage — implements ──────────────────────────────────────────────────────────────

    #[test]
    fn implements_external_interface_emits_inherits_edge() {
        let src = "class Dog implements Runnable {}";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "runnable", "m_dog", "inherits"),
            "external implements must emit inherits edge"
        );
    }

    #[test]
    fn implements_local_interface_emits_inherits_edge_with_stem_prefix() {
        let src = "interface Runnable {} class Dog implements Runnable {}";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "m_runnable", "m_dog", "inherits"),
            "local implements must use stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_implements_all_emit_inherits_edges() {
        let src = "class Foo implements Bar, Baz, Qux {}";
        let ex = extract(src, "m.java");
        assert!(has_edge(&ex, "bar", "m_foo", "inherits"));
        assert!(has_edge(&ex, "baz", "m_foo", "inherits"));
        assert!(has_edge(&ex, "qux", "m_foo", "inherits"));
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(inherits, 3, "expected 3 inherits edges from multiple implements");
    }

    #[test]
    fn both_extends_and_implements_emit_all_inherits_edges() {
        let src = "class Dog extends Animal implements Runnable {}";
        let ex = extract(src, "m.java");
        assert!(has_edge(&ex, "animal", "m_dog", "inherits"), "extends edge missing");
        assert!(has_edge(&ex, "runnable", "m_dog", "inherits"), "implements edge missing");
        let inherits: Vec<_> = ex.edges.iter().filter(|e| e.relation == "inherits").collect();
        assert_eq!(inherits.len(), 2, "expected exactly 2 inherits edges");
    }

    #[test]
    fn local_and_external_bases_mixed_classified_correctly() {
        let src = "class Base {} class Child extends Base implements External {}";
        let ex = extract(src, "mix.java");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
        assert!(
            has_edge(&ex, "external", "mix_child", "inherits"),
            "external base must use bare lowercased id"
        );
    }

    // ── I. Interface declarations ──────────────────────────────────────────────────────────────

    #[test]
    fn interface_declaration_emits_iface_node_and_contains_edge() {
        let src = "interface Printable {}";
        let ex = extract(src, "types.java");
        assert!(has_node(&ex, "types_printable"), "interface node missing");
        assert!(
            has_edge(&ex, "types", "types_printable", "contains"),
            "contains edge missing for interface"
        );
    }

    #[test]
    fn interface_name_is_lowercased_in_label() {
        let src = "interface HTTPHandler {}";
        let ex = extract(src, "iface.java");
        assert!(
            has_node(&ex, "iface_httphandler"),
            "interface label must be lowercased"
        );
    }

    #[test]
    fn multiple_interfaces_all_emitted() {
        let src = "interface A {} interface B {} interface C {}";
        let ex = extract(src, "ifaces.java");
        assert!(has_node(&ex, "ifaces_a"));
        assert!(has_node(&ex, "ifaces_b"));
        assert!(has_node(&ex, "ifaces_c"));
        let contains = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(contains, 3, "expected 3 contains edges");
    }

    #[test]
    fn interface_extends_other_interface_emits_inherits_edge() {
        let src = "interface Readable {} interface BufferedReadable extends Readable {}";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "m_readable", "m_bufferedreadable", "inherits"),
            "interface extends must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    // ── J. Enum declarations ───────────────────────────────────────────────────────────────────

    #[test]
    fn enum_declaration_emits_enum_node_and_contains_edge() {
        let src = "enum Color { RED, GREEN, BLUE }";
        let ex = extract(src, "enums.java");
        assert!(has_node(&ex, "enums_color"), "enum node missing");
        assert!(
            has_edge(&ex, "enums", "enums_color", "contains"),
            "contains edge missing for enum"
        );
    }

    #[test]
    fn enum_name_is_lowercased_in_label() {
        let src = "enum HTTPMethod { GET, POST, DELETE }";
        let ex = extract(src, "http.java");
        assert!(
            has_node(&ex, "http_httpmethod"),
            "enum label must be lowercased"
        );
    }

    #[test]
    fn enum_implementing_interface_emits_inherits_edge() {
        let src = "enum Planet implements Celestial { EARTH, MARS }";
        let ex = extract(src, "m.java");
        assert!(
            has_edge(&ex, "celestial", "m_planet", "inherits"),
            "enum implements must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    // ── K. Import handling ────────────────────────────────────────────────────────────────────

    #[test]
    fn import_emits_imports_from_edge() {
        let src = "import java.util.List;";
        let ex = extract(src, "f.java");
        assert!(
            has_edge(&ex, "f", "java.util.list", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_path_is_lowercased() {
        let src = "import java.util.List;";
        let ex = extract(src, "f.java");
        // Verify the target is lowercase (no uppercase letters)
        let found = ex
            .edges
            .iter()
            .any(|e| e.relation == "imports_from" && e.target == e.target.to_lowercase());
        assert!(found, "import path must be lowercased");
    }

    #[test]
    fn multiple_imports_all_emit_imports_from_edges() {
        let src =
            "import java.util.List;\nimport java.io.File;\nimport java.util.Map;";
        let ex = extract(src, "multi.java");
        assert!(has_edge(&ex, "multi", "java.util.list", "imports_from"), "List import missing");
        assert!(has_edge(&ex, "multi", "java.io.file", "imports_from"), "File import missing");
        assert!(has_edge(&ex, "multi", "java.util.map", "imports_from"), "Map import missing");
        let imports = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .count();
        assert_eq!(imports, 3, "expected 3 imports_from edges");
    }

    #[test]
    fn wildcard_import_emits_package_path_without_asterisk() {
        let src = "import java.util.*;";
        let ex = extract(src, "f.java");
        // The `.*` is an anonymous node; text_of on scoped_identifier gives "java.util"
        assert!(
            has_edge(&ex, "f", "java.util", "imports_from"),
            "wildcard import must emit package path without .*; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn static_import_emits_imports_from_edge() {
        let src = "import static java.util.Collections.sort;";
        let ex = extract(src, "f.java");
        let imports: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .collect();
        assert!(
            !imports.is_empty(),
            "static import must emit imports_from edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_short_path_emits_edge() {
        let src = "import javax.sql.DataSource;";
        let ex = extract(src, "db.java");
        assert!(
            has_edge(&ex, "db", "javax.sql.datasource", "imports_from"),
            "short dotted import path missing; edges: {:?}",
            ex.edges
        );
    }

    // ── L. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src =
            "class A { void foo() { new B().bar(); } } class B { void bar() {} }";
        let ex = extract(src, "neg.java");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "class Foo { int x = 1; int useX() { return x; } }";
        let ex = extract(src, "neg2.java");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_inherits_edges_when_no_heritage_declared() {
        let src = "class Isolated {} interface Pure {}";
        let ex = extract(src, "clean.java");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "no inherits edges expected when no extends/implements"
        );
    }

    // ── M. Edge properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "import java.util.List;\n",
            "class Foo extends Bar implements Baz { void m() {} }\n",
        );
        let ex = extract(src, "conf.java");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Extracted confidence; got {edge:?}"
            );
        }
    }

    // ── N. Source file field ───────────────────────────────────────────────────────────────────

    #[test]
    fn source_file_field_in_every_node() {
        let src = "class C { void m() {} }\ninterface I {}\nenum E { X }";
        let ex = extract(src, "traced.java");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("traced.java"),
                "source_file {:?} must contain 'traced.java'",
                n.source_file
            );
        }
    }

    // ── O. Span properties ────────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let ex = extract("class Foo {}", "span.java");
        let n = get_node(&ex, "span");
        assert_eq!(n.span.start_byte, 0, "file node span must start at byte 0");
    }

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let ex = extract("class Foo {}", "x.java");
        let n = get_node(&ex, "x_foo");
        assert_eq!(n.span.start_line, 1, "class on line 1 must have start_line=1");
    }

    #[test]
    fn class_on_third_line_has_correct_start_line() {
        let src = "\n\nclass Late {}";
        let ex = extract(src, "y.java");
        let n = get_node(&ex, "y_late");
        assert_eq!(n.span.start_line, 3, "class on line 3 must have start_line=3");
    }

    #[test]
    fn all_node_spans_are_well_formed() {
        let src = "class A { void run() {} } interface B {}";
        let ex = extract(src, "ws.java");
        for n in &ex.nodes {
            assert!(
                n.span.is_well_formed(),
                "span must be well-formed for {}: {n:?}",
                n.label
            );
        }
    }

    // ── P. Abstract class ─────────────────────────────────────────────────────────────────────

    #[test]
    fn abstract_class_emits_class_node_same_as_concrete() {
        let src = "abstract class Shape {}";
        let ex = extract(src, "shapes.java");
        assert!(
            has_node(&ex, "shapes_shape"),
            "abstract class node missing"
        );
        assert!(
            has_edge(&ex, "shapes", "shapes_shape", "contains"),
            "contains edge missing for abstract class"
        );
    }

    #[test]
    fn abstract_method_emits_method_node() {
        let src = "abstract class Shape { abstract void draw(); }";
        let ex = extract(src, "shapes.java");
        assert!(
            has_node(&ex, "shapes_shape_draw"),
            "abstract method node must be extracted"
        );
        assert!(has_edge(&ex, "shapes_shape", "shapes_shape_draw", "method"));
    }

    // ── Q. Underscore stripping in method id ──────────────────────────────────────────────────

    #[test]
    fn underscore_prefix_stripped_from_method_id() {
        let src = "class Foo { void _privateHelper() {} }";
        let ex = extract(src, "mod.java");
        assert!(
            has_node(&ex, "mod_foo_privatehelper"),
            "leading underscore must be stripped from method id; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── R. Totals / composite ─────────────────────────────────────────────────────────────────

    #[test]
    fn two_classes_two_methods_each_produce_correct_totals() {
        let src = concat!(
            "class A { void x() {} void y() {} }\n",
            "class B { void p() {} void q() {} }\n",
        );
        let ex = extract(src, "ab.java");
        // file(1) + class_a(1) + class_b(1) + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges (file→class only)");
    }

    #[test]
    fn mixed_class_interface_enum_import_all_emitted() {
        let src = concat!(
            "import java.util.List;\n",
            "class Parser {}\n",
            "interface Parseable {}\n",
            "enum Format { JSON, XML }\n",
        );
        let ex = extract(src, "mix.java");
        assert!(has_node(&ex, "mix"), "file node missing");
        assert!(has_node(&ex, "mix_parser"), "class node missing");
        assert!(has_node(&ex, "mix_parseable"), "interface node missing");
        assert!(has_node(&ex, "mix_format"), "enum node missing");
        assert!(has_edge(&ex, "mix", "mix_parser", "contains"));
        assert!(has_edge(&ex, "mix", "mix_parseable", "contains"));
        assert!(has_edge(&ex, "mix", "mix_format", "contains"));
        assert!(has_edge(&ex, "mix", "java.util.list", "imports_from"));
    }

    #[test]
    fn class_with_local_base_and_methods_fully_connected() {
        let src = concat!(
            "class Base { void init() {} }\n",
            "class Derived extends Base { void work() {} }\n",
        );
        let ex = extract(src, "chain.java");
        assert!(has_node(&ex, "chain_base"), "base class missing");
        assert!(has_node(&ex, "chain_derived"), "derived class missing");
        assert!(has_node(&ex, "chain_base_init"), "base method missing");
        assert!(has_node(&ex, "chain_derived_work"), "derived method missing");
        assert!(
            has_edge(&ex, "chain_base", "chain_derived", "inherits"),
            "inherits edge missing"
        );
    }

    // ── S. Empty class body ────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_class_body_produces_no_method_nodes() {
        let src = "class Empty {}";
        let ex = extract(src, "e.java");
        let extra_nodes: Vec<_> = ex
            .nodes
            .iter()
            .filter(|n| n.label != "e" && n.label != "e_empty")
            .collect();
        assert_eq!(
            extra_nodes.len(),
            0,
            "empty class body must produce no extra nodes"
        );
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "method").count(),
            0
        );
    }

    // ── T. Realistic Java service file ────────────────────────────────────────────────────────

    #[test]
    fn realistic_java_service_class_fully_extracted() {
        let src = concat!(
            "package com.example.service;\n",
            "import java.util.List;\n",
            "import java.io.IOException;\n",
            "public interface UserRepository {\n",
            "  List<String> findAll();\n",
            "}\n",
            "public abstract class AbstractService {\n",
            "  protected abstract void init();\n",
            "}\n",
            "public class UserService extends AbstractService implements UserRepository {\n",
            "  public UserService() {}\n",
            "  protected void init() {}\n",
            "  public List<String> findAll() { return null; }\n",
            "  private void helper() {}\n",
            "}\n",
        );
        let ex = extract(src, "UserService.java");
        assert!(has_node(&ex, "userservice"), "file node missing");
        assert!(
            has_node(&ex, "userservice_userrepository"),
            "UserRepository interface missing"
        );
        assert!(
            has_node(&ex, "userservice_abstractservice"),
            "AbstractService missing"
        );
        assert!(
            has_node(&ex, "userservice_userservice"),
            "UserService class missing"
        );
        assert!(
            has_node(&ex, "userservice_userservice_userservice"),
            "constructor missing"
        );
        assert!(
            has_node(&ex, "userservice_userservice_init"),
            "init method missing"
        );
        assert!(
            has_node(&ex, "userservice_userservice_findall"),
            "findAll method missing"
        );
        assert!(
            has_node(&ex, "userservice_userservice_helper"),
            "helper method missing"
        );
        // AbstractService (local) → UserService via extends
        assert!(
            has_edge(
                &ex,
                "userservice_abstractservice",
                "userservice_userservice",
                "inherits"
            ),
            "extends inherits missing"
        );
        // UserRepository (local) → UserService via implements
        assert!(
            has_edge(
                &ex,
                "userservice_userrepository",
                "userservice_userservice",
                "inherits"
            ),
            "implements inherits missing"
        );
        assert!(has_edge(&ex, "userservice", "java.util.list", "imports_from"));
        assert!(has_edge(
            &ex,
            "userservice",
            "java.io.ioexception",
            "imports_from"
        ));
    }

    // ── U. Unicode identifiers ─────────────────────────────────────────────────────────────────

    #[test]
    fn class_with_unicode_chars_in_name_handled_gracefully() {
        // Java identifiers are Unicode-aware; tree-sitter handles them correctly.
        let src = "class Données { void récupérer() {} }";
        let ex = extract(src, "data.java");
        // Verify no panic; file node must always be present.
        assert!(has_node(&ex, "data"), "file node must be present");
    }

    // ── V. Method span ────────────────────────────────────────────────────────────────────────

    #[test]
    fn method_span_end_is_ge_start() {
        let src = concat!(
            "class T {\n",
            "  void multi() {\n",
            "    int x = 1;\n",
            "    int y = 2;\n",
            "    System.out.println(x + y);\n",
            "  }\n",
            "}\n",
        );
        let ex = extract(src, "t.java");
        let n = get_node(&ex, "t_t_multi");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    #[test]
    fn method_span_is_well_formed_and_non_empty() {
        let src = "class Foo { void doSomething() { int x = 42; return; } }";
        let ex = extract(src, "foo.java");
        let n = get_node(&ex, "foo_foo_dosomething");
        assert!(n.span.is_well_formed(), "method span must be well-formed");
        assert!(!n.span.is_empty(), "method span must not be empty");
    }
}
