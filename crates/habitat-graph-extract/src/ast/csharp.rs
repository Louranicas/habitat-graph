//! C# AST extractor (`tree-sitter-c-sharp`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class, struct,
//! interface, and enum nodes `B_<name>`; method and constructor nodes `B_<cls>_<method>`.
//! Edges: `contains` (file→symbol), `method` (class/struct→method or constructor), `inherits`
//! (base→derived, via `base_list`, with local/external split), `imports_from`
//! (file→using-directive path). `calls` and `uses` are deliberately **NOT** emitted — they are
//! name-resolution heuristics that would not match graphify.
//!
//! Namespace declarations are recursed transparently: types declared inside a namespace receive
//! the same `B_<name>` label as top-level types (the namespace prefix is not incorporated into
//! node ids). Only plain `identifier` bases in a `base_list` produce `inherits` edges;
//! qualified names (`qualified_name`) and generic types (`generic_name`) are silently skipped
//! (consistent with the ts/java extractors' handling of unresolvable types).
//!
//! Aliased `using` directives (`using X = Y.Z;`) are skipped. Plain and `using static`
//! directives both emit an `imports_from` edge to the lowercased namespace path.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from C# source using `tree-sitter-c-sharp`, in graphify's
/// qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (lowercased stem): one file node `B`; class, struct, interface,
/// and enum nodes `B_<name>`; method and constructor nodes `B_<cls>_<method>`. Edges: `contains`
/// (file→symbol), `method` (class/struct→method or constructor), `inherits` (base→derived,
/// local/external split from `base_list`), `imports_from` (file→using path).
/// `calls`/`uses` are deliberately omitted.
///
/// Namespace declarations are recursed transparently. An empty file produces exactly one node
/// (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct CsharpExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Returns the method-id component `m` of the qualified label `B_C_m`.
///
/// Strips all leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting only of
/// underscores).
///
/// Examples: `_Init` → `"init"`, `__ctor__` → `"ctor"`, `MyMethod` → `"mymethod"`,
/// `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Collects lowercased names of all locally declared classes, structs, and interfaces.
///
/// Recurses into `namespace_declaration` bodies so that types nested inside a namespace are
/// also included in the local-type set. This is used in the first pass to distinguish local
/// bases (same file) from external ones when emitting `inherits` edges.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_local_types(root, source, &mut names);
    names
}

/// Recursive inner function for `local_type_names`.
fn collect_local_types(node: tree_sitter::Node<'_>, source: &[u8], names: &mut HashSet<String>) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "class_declaration" | "interface_declaration" | "struct_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    names.insert(text_of(source, &name_node).to_lowercase());
                }
            }
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                if let Some(body) = child.child_by_field_name("body") {
                    collect_local_types(body, source, names);
                }
                // file-scoped namespaces: body may be the compilation_unit itself; walk children
                collect_local_types(child, source, names);
            }
            "declaration_list" => {
                collect_local_types(child, source, names);
            }
            _ => {}
        }
    }
}

/// Dispatches one top-level or namespace-body declaration node to the appropriate extractor.
///
/// Handles `class_declaration`, `struct_declaration`, `interface_declaration`,
/// `enum_declaration`, `namespace_declaration`, `file_scoped_namespace_declaration`, and
/// `using_directive`. All other node kinds are silently ignored.
fn dispatch_decl(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    match node.kind() {
        "class_declaration" | "struct_declaration" => {
            extract_class_or_struct(node, source, b, source_file, local_types, result);
        }
        "interface_declaration" => {
            extract_interface(node, source, b, source_file, result);
        }
        "enum_declaration" => {
            extract_enum(node, source, b, source_file, result);
        }
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            extract_namespace(node, source, b, source_file, local_types, result);
        }
        "using_directive" | "global_statement" => {
            // global_statement is a C# 9+ top-level statement; treat any using_directive inside
            // a global_statement transparently.
            extract_using_child(node, source, b, result);
        }
        _ => {}
    }
}

/// Dispatches `using_directive` nodes that may appear as direct top-level children or wrapped
/// inside a `global_statement`.
fn extract_using_child(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    result: &mut Extraction,
) {
    if node.kind() == "using_directive" {
        extract_using(node, source, b, result);
    } else {
        // Recurse one level to find a using_directive inside e.g. global_statement.
        for i in 0..node.named_child_count() {
            let Some(child) = node.named_child(i) else {
                continue;
            };
            if child.kind() == "using_directive" {
                extract_using(&child, source, b, result);
            }
        }
    }
}

/// Extracts a `class_declaration` or `struct_declaration` into `result`.
///
/// Emits:
/// - A class/struct node `B_c` (lowercased name) and a `contains` edge from `B`.
/// - One `inherits` edge per plain `identifier` in the `base_list` named child (qualified names
///   and generic types are silently skipped).
/// - One method node `B_c_m` and a `method` edge per `method_declaration` or
///   `constructor_declaration` in the `body` (`declaration_list`).
fn extract_class_or_struct(
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

    // Walk named children to find base_list — in tree-sitter-c-sharp it is not in a field.
    for idx in 0..node.named_child_count() {
        let Some(child) = node.named_child(idx) else {
            continue;
        };
        if child.kind() == "base_list" {
            extract_base_list(&child, source, b, &class_label, local_types, result);
        }
    }

    // Extract methods/constructors from the declaration_list body.
    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(&body, source, b, &c, &class_label, source_file, result);
    }
}

/// Extracts `inherits` edges from a `base_list` node.
///
/// Emits `inherits(base_id → class_label)` for each plain `identifier` child of `base_list`.
/// Qualified names (`qualified_name`) and generic types (`generic_name`) are silently skipped
/// — they are not resolvable without external type information.
///
/// The `base_id` follows the local/external split: a base whose lowercased name is in
/// `local_types` (same file) gets `"b_<base>"`, an external base gets `"<base>"`.
fn extract_base_list(
    base_list: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    class_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for i in 0..base_list.named_child_count() {
        let Some(child) = base_list.named_child(i) else {
            continue;
        };
        // Only plain identifier bases are resolvable without type-system knowledge.
        if child.kind() == "identifier" {
            let base_lower = text_of(source, &child).to_lowercase();
            let base_id = if local_types.contains(&base_lower) {
                format!("{b}_{base_lower}")
            } else {
                base_lower
            };
            result.edges.push(RawEdge {
                source: base_id,
                target: class_label.to_owned(),
                relation: "inherits".to_owned(),
                confidence: Confidence::Extracted,
            });
        }
    }
}

/// Extracts `method_declaration` and `constructor_declaration` children of a `declaration_list`.
///
/// Emits a method node `B_c_m` and a `method` edge from `class_label` for each qualifying
/// member. The `method_id` helper strips leading/trailing `_` before lowercasing. Other member
/// kinds (properties, fields, events, nested types) are silently skipped.
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
        let Some(member) = body.named_child(idx) else {
            continue;
        };
        if !matches!(
            member.kind(),
            "method_declaration" | "constructor_declaration"
        ) {
            continue;
        }
        let Some(name_node) = member.child_by_field_name("name") else {
            continue;
        };
        let raw_method = text_of(source, &name_node);
        let m = method_id(&raw_method);
        let method_label = format!("{b}_{c}_{m}");
        result.nodes.push(RawNode {
            label: method_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&member),
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
/// Emits an interface node `B_<iface>` and a `contains` edge from `B`. Interface members are
/// not individually emitted (the taxonomy treats interfaces as monolithic symbols). The
/// interface's own `base_list` (e.g. `interface IFoo : IBar`) is silently ignored — in
/// graphify's taxonomy interface-extends-interface is not captured at this tier.
fn extract_interface(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
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
        target: iface_label,
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Extracts one `enum_declaration` into `result`.
///
/// Emits an enum node `B_<enum>` and a `contains` edge from `B`. Any `base_list` on the enum
/// (which specifies the underlying integer type, e.g. `enum Status : byte`) is **not** treated
/// as inheritance — no `inherits` edges are emitted for enums.
fn extract_enum(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
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
        target: enum_label,
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Recurses into a `namespace_declaration` or `file_scoped_namespace_declaration` body.
///
/// The namespace name is not incorporated into node labels — types inside a namespace get the
/// same `B_<name>` label as top-level types (the namespace is transparent to the taxonomy).
fn extract_namespace(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    // Standard namespace: body is a declaration_list in the "body" field.
    if let Some(body) = node.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            let Some(child) = body.named_child(i) else {
                continue;
            };
            dispatch_decl(&child, source, b, source_file, local_types, result);
        }
        return;
    }
    // File-scoped namespace (C# 10+): declarations follow directly as named children of
    // the namespace node, without a declaration_list wrapper.
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        // Skip the namespace name itself.
        if matches!(child.kind(), "identifier" | "qualified_name") {
            continue;
        }
        dispatch_decl(&child, source, b, source_file, local_types, result);
    }
}

/// Extracts a `using_directive` into `result` as an `imports_from` edge.
///
/// Emits `imports_from(B → module)` where `module` is the lowercased text of the namespace
/// identifier or qualified name. Aliased directives (`using X = Y.Z;`) are skipped. In
/// tree-sitter-c-sharp v0.23.x the alias is not wrapped in a `name_equals` node — it appears
/// as the first plain `identifier` named child, with the actual namespace as the second
/// name-like child. Detection: if there are **two or more** name-like named children (`identifier`
/// or `qualified_name` or `alias_qualified_name`), the directive is aliased and is skipped.
/// With exactly one name-like child, that child IS the namespace.
///
/// The `static` modifier and `global` keyword are anonymous tokens (not named children) and are
/// therefore transparent to this logic. `using static System.Math;` correctly emits
/// `imports_from(B → "system.math")`.
///
/// Resolution: if the grammar exposes a `name` field, it is tried first; otherwise the single
/// name-like named child is used.
fn extract_using(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    // Collect all name-like named children in order.
    let mut name_nodes: Vec<tree_sitter::Node<'_>> = Vec::new();
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            // `name_equals` wraps the alias in some grammar versions.
            "name_equals" => return,
            "qualified_name" | "identifier" | "alias_qualified_name" => {
                name_nodes.push(child);
            }
            _ => {}
        }
    }

    // Two or more name-like children → aliased directive (`using X = Y.Z;`) — skip.
    if name_nodes.len() >= 2 {
        return;
    }

    // Try the grammar's `name` field first (some grammar versions expose it directly).
    if let Some(name_node) = node.child_by_field_name("name") {
        let module = text_of(source, &name_node).to_lowercase();
        if !module.is_empty() {
            push_imports_from(b, &module, result);
        }
        return;
    }

    // Use the single collected name-like child.
    if let Some(name_node) = name_nodes.into_iter().next() {
        let module = text_of(source, &name_node).to_lowercase();
        if !module.is_empty() {
            push_imports_from(b, &module, result);
        }
    }
}

/// Pushes one `imports_from` edge into `result`.
fn push_imports_from(b: &str, module: &str, result: &mut Extraction) {
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: module.to_owned(),
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for CsharpExtractor {
    fn language(&self) -> &'static str {
        "csharp"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cs"]
    }

    /// Extracts C# nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: file node `B` (lowercased stem); class, struct,
    /// interface, and enum nodes `B_<name>`; method/constructor nodes `B_<cls>_<method>`; with
    /// `contains`, `method`, `inherits`, and `imports_from` edges. `calls`/`uses` are deliberately
    /// omitted. Namespace declarations are recursed transparently. An empty file produces exactly
    /// one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The C# grammar could not be installed on the parser (should never occur with a correctly
    ///   linked `tree-sitter-c-sharp`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid C# syntax;
    ///   tree-sitter is error-tolerant and always returns a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
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

        // Pass 2: walk compilation_unit top-level children.
        for idx in 0..root.named_child_count() {
            let Some(child) = root.named_child(idx) else {
                continue;
            };
            dispatch_decl(&child, source, &b, &source_file, &local_types, &mut result);
        }

        Ok(result)
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::CsharpExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if the file is named `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        CsharpExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("csharp extractor failed on {filename}: {e}"))
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

    /// Returns the node with the given label; panics with context if absent.
    fn node<'e>(ex: &'e Extraction, label: &str) -> &'e habitat_graph_core::RawNode {
        ex.nodes
            .iter()
            .find(|n| n.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "node '{label}' not found; present: {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    // ── A. Empty / trivial ─────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "Program.cs");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "program");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn whitespace_only_source_yields_only_file_node() {
        let ex = extract("   \n\t\n   ", "empty.cs");
        assert_eq!(
            ex.nodes.len(),
            1,
            "whitespace-only must yield exactly 1 node"
        );
        assert_eq!(ex.nodes[0].label, "empty");
    }

    #[test]
    fn malformed_source_returns_ok_file_node_present() {
        // tree-sitter is error-tolerant: always produces a partial tree even for garbage.
        let ex = extract("@@@!!! NOT VALID C# ~~~", "broken.cs");
        assert!(
            has_node(&ex, "broken"),
            "file node must be present for garbled source"
        );
    }

    // ── B. File stem handling ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HttpClient.cs");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_all_caps_fully_lowercased() {
        let ex = extract("", "APIUTILS.cs");
        assert_eq!(ex.nodes[0].label, "apiutils");
    }

    #[test]
    fn file_stem_preserves_underscores_and_digits() {
        let ex = extract("", "my_module2.cs");
        assert_eq!(ex.nodes[0].label, "my_module2");
    }

    #[test]
    fn language_slug_is_csharp() {
        assert_eq!(CsharpExtractor.language(), "csharp");
    }

    #[test]
    fn extension_is_cs() {
        let exts = CsharpExtractor.extensions();
        assert_eq!(exts, &["cs"], "only .cs extension must be registered");
    }

    // ── C. Class declarations ──────────────────────────────────────────────────────────────────

    #[test]
    fn class_declaration_emits_class_node_and_contains_edge() {
        let ex = extract("class Dog { }", "animals.cs");
        assert!(has_node(&ex, "animals_dog"), "class node missing");
        assert!(
            has_edge(&ex, "animals", "animals_dog", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_fully_lowercased() {
        let ex = extract("class HTTPClient { }", "client.cs");
        assert!(
            has_node(&ex, "client_httpclient"),
            "class label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_top_level_classes_all_emitted() {
        let ex = extract("class A { } class B { } class C { }", "classes.cs");
        assert!(has_node(&ex, "classes_a"), "A missing");
        assert!(has_node(&ex, "classes_b"), "B missing");
        assert!(has_node(&ex, "classes_c"), "C missing");
        let symbol_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "classes").collect();
        assert_eq!(symbol_nodes.len(), 3, "expected 3 class nodes");
    }

    #[test]
    fn class_with_no_heritage_produces_no_inherits_edges() {
        let ex = extract("class Standalone { }", "s.cs");
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(
            inherits, 0,
            "class with no base list must have 0 inherits edges"
        );
    }

    // ── D. Struct declarations ─────────────────────────────────────────────────────────────────

    #[test]
    fn struct_declaration_emits_struct_node_and_contains_edge() {
        let ex = extract("struct Point { }", "geometry.cs");
        assert!(has_node(&ex, "geometry_point"), "struct node missing");
        assert!(
            has_edge(&ex, "geometry", "geometry_point", "contains"),
            "contains edge missing for struct"
        );
    }

    #[test]
    fn struct_name_is_fully_lowercased() {
        let ex = extract("struct Vec3D { }", "math.cs");
        assert!(
            has_node(&ex, "math_vec3d"),
            "struct label must be lowercased"
        );
    }

    // ── E. Interface declarations ──────────────────────────────────────────────────────────────

    #[test]
    fn interface_declaration_emits_iface_node_and_contains_edge() {
        let ex = extract("interface IRunnable { }", "contracts.cs");
        assert!(
            has_node(&ex, "contracts_irunnable"),
            "interface node missing"
        );
        assert!(
            has_edge(&ex, "contracts", "contracts_irunnable", "contains"),
            "contains edge missing for interface"
        );
    }

    #[test]
    fn interface_name_is_fully_lowercased() {
        let ex = extract("interface IHTTPHandler { }", "iface.cs");
        assert!(
            has_node(&ex, "iface_ihttphandler"),
            "interface label must be lowercased"
        );
    }

    #[test]
    fn multiple_interfaces_all_emitted() {
        let ex = extract(
            "interface IA { } interface IB { } interface IC { }",
            "ifaces.cs",
        );
        assert!(has_node(&ex, "ifaces_ia"));
        assert!(has_node(&ex, "ifaces_ib"));
        assert!(has_node(&ex, "ifaces_ic"));
        let contains = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(contains, 3, "expected 3 contains edges");
    }

    // ── F. Enum declarations ───────────────────────────────────────────────────────────────────

    #[test]
    fn enum_declaration_emits_enum_node_and_contains_edge() {
        let ex = extract("enum Color { Red, Green, Blue }", "enums.cs");
        assert!(has_node(&ex, "enums_color"), "enum node missing");
        assert!(
            has_edge(&ex, "enums", "enums_color", "contains"),
            "contains edge missing for enum"
        );
    }

    #[test]
    fn enum_name_is_fully_lowercased() {
        let ex = extract("enum HTTPMethod { GET, POST, PUT }", "http.cs");
        assert!(
            has_node(&ex, "http_httpmethod"),
            "enum label must be lowercased"
        );
    }

    #[test]
    fn enum_with_underlying_type_does_not_emit_inherits_edge() {
        // `enum Status : byte` — the `: byte` is the underlying type, NOT inheritance.
        let ex = extract("enum Status : byte { Active, Inactive }", "status.cs");
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(
            inherits, 0,
            "enum underlying-type base_list must not produce inherits edges"
        );
    }

    // ── G. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_emits_method_node_and_method_edge() {
        let ex = extract("class Foo { void Bark() { } }", "dog.cs");
        assert!(has_node(&ex, "dog_foo_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_foo", "dog_foo_bark", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn method_name_is_fully_lowercased() {
        let ex = extract("class Foo { void MyMethod() { } }", "m.cs");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_methods_in_class_all_emitted() {
        let ex = extract(
            "class Foo { void Alpha() { } int Beta() { return 0; } void Gamma() { } }",
            "methods.cs",
        );
        assert!(has_node(&ex, "methods_foo_alpha"), "Alpha missing");
        assert!(has_node(&ex, "methods_foo_beta"), "Beta missing");
        assert!(has_node(&ex, "methods_foo_gamma"), "Gamma missing");
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        assert_eq!(method_edges, 3, "expected 3 method edges");
    }

    #[test]
    fn method_qualified_id_format_b_cls_method() {
        let ex = extract("class MyClass { void DoWork() { } }", "mymod.cs");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method label must match b_cls_method format"
        );
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let ex = extract("class Dog { void Run() { } }", "pet.cs");
        assert!(has_edge(&ex, "pet_dog", "pet_dog_run", "method"));
        assert!(
            !has_edge(&ex, "pet", "pet_dog_run", "method"),
            "method edge source must be class label, not file label"
        );
    }

    #[test]
    fn underscore_prefix_stripped_from_method_id() {
        let ex = extract("class Foo { void _Private() { } }", "mod.cs");
        assert!(
            has_node(&ex, "mod_foo_private"),
            "leading underscore must be stripped; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn constructor_is_extracted_as_method() {
        let ex = extract("class Widget { Widget() { } }", "w.cs");
        assert!(
            has_node(&ex, "w_widget_widget"),
            "constructor must be extracted as method; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "w_widget", "w_widget_widget", "method"));
    }

    #[test]
    fn struct_methods_emitted_same_as_class_methods() {
        let ex = extract("struct Point { void Scale(float f) { } }", "geo.cs");
        assert!(
            has_node(&ex, "geo_point_scale"),
            "struct method node missing"
        );
        assert!(has_edge(&ex, "geo_point", "geo_point_scale", "method"));
    }

    // ── H. Inheritance via base_list ───────────────────────────────────────────────────────────

    #[test]
    fn class_extends_external_emits_inherits_edge_with_bare_base_id() {
        let ex = extract("class Dog : Animal { }", "m.cs");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external base must yield bare lowercased base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_extends_local_emits_inherits_edge_with_stem_prefix() {
        let ex = extract("class Base { } class Child : Base { }", "m.cs");
        assert!(
            has_edge(&ex, "m_base", "m_child", "inherits"),
            "local base must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_implements_external_interface_emits_inherits_edge() {
        let ex = extract("class Dog : IRunnable { }", "m.cs");
        assert!(
            has_edge(&ex, "irunnable", "m_dog", "inherits"),
            "external implements must emit inherits edge"
        );
    }

    #[test]
    fn class_implements_local_interface_emits_inherits_edge_with_stem_prefix() {
        let ex = extract("interface IFoo { } class Bar : IFoo { }", "m.cs");
        assert!(
            has_edge(&ex, "m_ifoo", "m_bar", "inherits"),
            "local interface must use stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_with_multiple_bases_all_emit_inherits_edges() {
        let ex = extract("class Dog : Animal, IRunnable, ICloneable { }", "m.cs");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "Animal edge missing"
        );
        assert!(
            has_edge(&ex, "irunnable", "m_dog", "inherits"),
            "IRunnable edge missing"
        );
        assert!(
            has_edge(&ex, "icloneable", "m_dog", "inherits"),
            "ICloneable edge missing"
        );
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(inherits, 3, "expected 3 inherits edges");
    }

    #[test]
    fn local_and_external_bases_mixed_classified_correctly() {
        let ex = extract("class Base { } class Child : Base, IExternal { }", "mix.cs");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
        assert!(
            has_edge(&ex, "iexternal", "mix_child", "inherits"),
            "external base must use bare lowercased id"
        );
    }

    #[test]
    fn struct_with_interface_emits_inherits_edge() {
        let ex = extract("struct MyStruct : IDisposable { }", "structs.cs");
        assert!(
            has_edge(&ex, "idisposable", "structs_mystruct", "inherits"),
            "struct implementing interface must emit inherits edge"
        );
    }

    // ── I. Using directives ────────────────────────────────────────────────────────────────────

    #[test]
    fn using_simple_name_emits_imports_from_edge() {
        let ex = extract("using System;", "prog.cs");
        assert!(
            has_edge(&ex, "prog", "system", "imports_from"),
            "simple using must emit imports_from; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn using_qualified_name_emits_imports_from_edge() {
        let ex = extract("using System.Collections.Generic;", "prog.cs");
        // The qualified name is lowercased; tree-sitter gives us the full dotted text.
        assert!(
            ex.edges
                .iter()
                .any(|e| e.relation == "imports_from" && e.source == "prog"),
            "qualified using must emit imports_from; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn using_source_is_lowercased() {
        let ex = extract("using SYSTEM;", "prog.cs");
        // Even though "SYSTEM" is unusual C#, tree-sitter parses it and we lowercase.
        // The edge target must be lowercase.
        for edge in ex.edges.iter().filter(|e| e.relation == "imports_from") {
            assert_eq!(
                edge.target,
                edge.target.to_lowercase(),
                "imports_from target must be lowercased; got {:?}",
                edge.target
            );
        }
    }

    #[test]
    fn multiple_usings_all_emit_imports_from_edges() {
        let src = "using System;\nusing System.IO;\nusing System.Text;";
        let ex = extract(src, "prog.cs");
        let imports = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .count();
        assert_eq!(
            imports, 3,
            "three using directives must emit 3 imports_from edges"
        );
    }

    #[test]
    fn aliased_using_does_not_emit_imports_from_edge() {
        // `using Alias = Something;` should be skipped.
        let ex = extract("using IO = System.IO;", "prog.cs");
        let imports = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .count();
        assert_eq!(
            imports, 0,
            "aliased using directive must not emit imports_from edge; edges: {:?}",
            ex.edges
        );
    }

    // ── J. Namespace recursion ─────────────────────────────────────────────────────────────────

    #[test]
    fn namespace_declarations_are_recursed() {
        let ex = extract("namespace MyNs { class Foo { } }", "ns.cs");
        assert!(
            has_node(&ex, "ns_foo"),
            "type in namespace must be extracted; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(
            has_edge(&ex, "ns", "ns_foo", "contains"),
            "contains edge for type in namespace missing"
        );
    }

    #[test]
    fn types_in_namespace_have_same_label_format_as_top_level_types() {
        let ex_ns = extract("namespace Ns { class Bar { } }", "f.cs");
        let ex_top = extract("class Bar { }", "f.cs");
        // Both should produce a node labeled "f_bar".
        assert!(
            has_node(&ex_ns, "f_bar"),
            "namespace-nested type label must match top-level format"
        );
        assert!(
            has_node(&ex_top, "f_bar"),
            "top-level type label must match expected format"
        );
    }

    #[test]
    fn multiple_types_in_namespace_all_extracted() {
        let src = "namespace N { class A { } interface IA { } enum E { X } }";
        let ex = extract(src, "ns.cs");
        assert!(has_node(&ex, "ns_a"), "class in namespace missing");
        assert!(has_node(&ex, "ns_ia"), "interface in namespace missing");
        assert!(has_node(&ex, "ns_e"), "enum in namespace missing");
    }

    // ── K. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = "class A { void M() { new B().Go(); } } class B { void Go() { } }";
        let ex = extract(src, "neg.cs");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "class Config { public static int MaxSize = 100; } class App { void Run() { int n = Config.MaxSize; } }";
        let ex = extract(src, "neg2.cs");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn methods_get_method_edge_not_contains_edge() {
        let ex = extract("class Foo { void Go() { } }", "f.cs");
        let method_via_contains = ex
            .edges
            .iter()
            .any(|e| e.relation == "contains" && e.target == "f_foo_go");
        assert!(
            !method_via_contains,
            "method node must NOT appear via a contains edge"
        );
        assert!(
            has_edge(&ex, "f_foo", "f_foo_go", "method"),
            "method node must appear via a method edge"
        );
    }

    // ── L. Edge properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "using System;\n",
            "class Foo : Bar { void M() { } }\n",
            "interface IBar { }\n",
            "enum Status { Ok }\n",
        );
        let ex = extract(src, "conf.cs");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Extracted confidence; got {edge:?}"
            );
        }
    }

    // ── M. Span properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let ex = extract("class Foo { }", "span.cs");
        let n = node(&ex, "span");
        assert_eq!(n.span.start_byte, 0, "file node span must start at byte 0");
    }

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let ex = extract("class Foo { }", "x.cs");
        let n = node(&ex, "x_foo");
        assert_eq!(
            n.span.start_line, 1,
            "class on line 1 must have start_line=1"
        );
    }

    #[test]
    fn class_on_third_line_has_correct_start_line() {
        let src = "\n\nclass Late { }";
        let ex = extract(src, "y.cs");
        let n = node(&ex, "y_late");
        assert_eq!(
            n.span.start_line, 3,
            "class on line 3 must have start_line=3"
        );
    }

    #[test]
    fn all_node_spans_well_formed_for_non_trivial_source() {
        let src = "class A { void Run() { } } interface IA { }";
        let ex = extract(src, "ws.cs");
        for n in &ex.nodes {
            assert!(
                n.span.is_well_formed(),
                "span must be well-formed for {}: {n:?}",
                n.label
            );
        }
    }

    #[test]
    fn source_file_field_in_every_node() {
        let src = "class C { void M() { } }\nenum E { X }";
        let ex = extract(src, "traced.cs");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("traced.cs"),
                "source_file {:?} must contain 'traced.cs'",
                n.source_file
            );
        }
    }

    #[test]
    fn method_span_end_is_ge_start() {
        let src = "class Foo { void Big() {\n  int x = 1;\n  int y = 2;\n} }";
        let ex = extract(src, "sp.cs");
        let n = node(&ex, "sp_foo_big");
        assert!(
            n.span.end_line >= n.span.start_line,
            "method span end_line must be >= start_line: {n:?}"
        );
    }

    // ── N. Composite / integration tests ──────────────────────────────────────────────────────

    #[test]
    fn mixed_class_interface_enum_using_all_emitted() {
        let src = concat!(
            "using System;\n",
            "interface ILogger { }\n",
            "enum Level { Info, Warn }\n",
            "class App : ILogger { void Log() { } }\n",
        );
        let ex = extract(src, "app.cs");
        assert!(has_node(&ex, "app"), "file node missing");
        assert!(has_node(&ex, "app_ilogger"), "interface missing");
        assert!(has_node(&ex, "app_level"), "enum missing");
        assert!(has_node(&ex, "app_app"), "class missing");
        assert!(has_node(&ex, "app_app_log"), "method missing");
        assert!(has_edge(&ex, "app", "app_ilogger", "contains"));
        assert!(has_edge(&ex, "app", "app_level", "contains"));
        assert!(has_edge(&ex, "app", "app_app", "contains"));
        assert!(has_edge(&ex, "app_app", "app_app_log", "method"));
        assert!(has_edge(&ex, "app", "system", "imports_from"));
    }

    #[test]
    fn class_with_methods_and_inheritance_fully_connected() {
        let ex = extract(
            "class Animal { void Breathe() { } } class Dog : Animal { void Bark() { } }",
            "chain.cs",
        );
        assert!(has_node(&ex, "chain_animal"), "animal class missing");
        assert!(has_node(&ex, "chain_dog"), "dog class missing");
        assert!(
            has_node(&ex, "chain_animal_breathe"),
            "breathe method missing"
        );
        assert!(has_node(&ex, "chain_dog_bark"), "bark method missing");
        assert!(
            has_edge(&ex, "chain_animal", "chain_dog", "inherits"),
            "inherits edge missing"
        );
    }

    #[test]
    fn two_classes_two_methods_each_correct_totals() {
        let src = "class A { void X() { } void Y() { } } class B { void P() { } void Q() { } }";
        let ex = extract(src, "ab.cs");
        // file(1) + 2 classes + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges");
    }

    #[test]
    fn complex_file_all_symbol_types_correct_node_set() {
        let src = concat!(
            "using System;\n",
            "using System.IO;\n",
            "namespace App {\n",
            "  interface ILogger { }\n",
            "  enum Level { Info }\n",
            "  class Base { void Init() { } }\n",
            "  class Service : Base, ILogger {\n",
            "    Service() { }\n",
            "    void Log(Level lvl) { }\n",
            "  }\n",
            "  struct Config { }\n",
            "}\n",
        );
        let ex = extract(src, "app.cs");
        // file + ilogger + level + base + service + config + init + ctor + log = 9 nodes
        assert!(has_node(&ex, "app"), "file node");
        assert!(has_node(&ex, "app_ilogger"), "interface");
        assert!(has_node(&ex, "app_level"), "enum");
        assert!(has_node(&ex, "app_base"), "base class");
        assert!(has_node(&ex, "app_service"), "service class");
        assert!(has_node(&ex, "app_config"), "struct");
        assert!(has_node(&ex, "app_base_init"), "base method");
        assert!(has_node(&ex, "app_service_service"), "constructor");
        assert!(has_node(&ex, "app_service_log"), "log method");
        // inherits edges
        assert!(
            has_edge(&ex, "app_base", "app_service", "inherits"),
            "Base inherits edge"
        );
        assert!(
            has_edge(&ex, "app_ilogger", "app_service", "inherits"),
            "ILogger inherits edge"
        );
        // imports
        assert!(ex
            .edges
            .iter()
            .any(|e| e.relation == "imports_from" && e.source == "app"));
    }

    #[test]
    fn namespace_with_inheriting_classes_inherits_uses_local_split() {
        let src = "namespace N { class Base { } class Child : Base { } }";
        let ex = extract(src, "ns.cs");
        assert!(
            has_edge(&ex, "ns_base", "ns_child", "inherits"),
            "local base in namespace must use stem-prefixed id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn method_qualified_id_b_cls_method_pattern_verified() {
        let ex = extract("class Alpha { void Beta() { } }", "gamma.cs");
        assert!(
            has_node(&ex, "gamma_alpha_beta"),
            "method qualified id must be b_cls_method"
        );
    }

    #[test]
    fn double_underscore_method_stripped_correctly() {
        let ex = extract("class Foo { void __init__() { } }", "mod.cs");
        // After stripping leading/trailing `_`: "init"
        assert!(
            has_node(&ex, "mod_foo_init"),
            "double-underscore method must strip to inner name; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn class_and_struct_with_same_stem_and_method_count_correct() {
        let src = "class C1 { void M1() { } void M2() { } } struct S1 { void M3() { } }";
        let ex = extract(src, "mixed.cs");
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        assert_eq!(
            method_edges, 3,
            "expected 3 method edges (2 class + 1 struct)"
        );
    }

    #[test]
    fn file_node_always_present_for_using_only_file() {
        let ex = extract("using System;\nusing System.Linq;", "imports.cs");
        assert!(has_node(&ex, "imports"), "file node must be present");
        let imports = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .count();
        assert_eq!(imports, 2, "expected 2 imports_from edges");
    }

    #[test]
    fn contains_edge_count_matches_symbol_count() {
        let src = "class A { } interface IB { } enum C { X } struct D { }";
        let ex = extract(src, "counts.cs");
        let contains = ex.edges.iter().filter(|e| e.relation == "contains").count();
        // 4 symbols → 4 contains edges
        assert_eq!(contains, 4, "expected 4 contains edges for 4 symbols");
    }
}
