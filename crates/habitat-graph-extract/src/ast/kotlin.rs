//! Kotlin AST extractor (`tree-sitter-kotlin-ng`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class, abstract-class,
//! data-class, sealed-class, and enum-class nodes `B_<name>` (all from `class_declaration`);
//! interface nodes `B_<name>` (from `interface_declaration` or `class_declaration` with an
//! `interface` keyword, depending on grammar variant); object-declaration nodes `B_<name>`;
//! top-level function nodes `B_<fn>`; method nodes `B_<cls>_<method>`. Edges: `contains`
//! (file→symbol), `method` (class/object→method), `inherits` (base→derived from `:` delegation
//! specifiers, with local/external split), `imports_from` (file→package path). `calls` and `uses`
//! are deliberately **NOT** emitted — they are name-resolution heuristics that would not match
//! graphify's committed goldens.
//!
//! Both `.kt` (Kotlin source) and `.kts` (Kotlin Script) extensions are supported; they use the
//! same grammar. Primary constructors in the class header are **not** emitted as separate method
//! nodes; `secondary_constructor` declarations appearing in the class body are emitted as method
//! nodes with the fixed name component `"constructor"`.
//!
//! Import extraction is text-based: the `import_header` node text is stripped of the leading
//! `"import "` keyword, any trailing `" as Alias"` renaming, and any trailing `".*"` wildcard
//! suffix. Import headers wrapped in an `import_list` container node (grammar variant) are also
//! handled.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from Kotlin source using `tree-sitter-kotlin-ng`, in graphify's
/// qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (lowercased stem): one file node `B`; class, interface, and
/// object nodes `B_<name>`; function nodes `B_<fn>`; method nodes `B_<cls>_<method>`. Edges:
/// `contains` (file→symbol), `method` (class/object→method), `inherits` (base→derived, with
/// local/external split), `imports_from` (file→import path). `calls`/`uses` are deliberately
/// omitted.
///
/// An empty file produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct KotlinExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Returns the method-id component `m` of the qualified label `B_C_m`.
///
/// Strips all leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting only of
/// underscores).
///
/// Examples: `_init` → `"init"`, `__companion` → `"companion"`, `myFun` → `"myfun"`,
/// `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Extracts the lowercased simple name from a declaration node.
///
/// Tries `child_by_field_name("name")` first (the tree-sitter-kotlin-ng grammar exposes the
/// name as an explicit field on class, object, and function declarations). Falls back to
/// scanning named children for the first `type_identifier` or `simple_identifier` node (covers
/// grammar variants that omit the explicit field).
///
/// Returns `None` when neither strategy finds a non-empty name, causing the caller to silently
/// skip the declaration.
fn decl_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        let text = text_of(source, &n);
        if !text.is_empty() {
            return Some(text.to_lowercase());
        }
    }
    // Fallback: first type_identifier or simple_identifier named child.
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "type_identifier" | "simple_identifier") {
            let text = text_of(source, &child);
            if !text.is_empty() {
                return Some(text.to_lowercase());
            }
        }
    }
    None
}

/// Builds the set of lowercased names of top-level class-like declarations in this file.
///
/// Covers `class_declaration`, `interface_declaration`, and `object_declaration` at the root
/// level. This first pass is used to distinguish local bases (same file) from external ones
/// when emitting `inherits` edges: a local base `Foo` in file with stem `b` gets
/// `base_id = "b_foo"`, while an external base gets `base_id = "foo"`.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        if matches!(
            child.kind(),
            "class_declaration" | "interface_declaration" | "object_declaration"
        ) {
            if let Some(name) = decl_name(&child, source) {
                names.insert(name);
            }
        }
    }
    names
}

/// Extracts one `import_header` node into an `imports_from` edge.
///
/// The node's source text is expected to start with `"import "`. The package path is produced
/// by stripping that prefix, stripping any trailing `" as Alias"` renaming, and stripping any
/// trailing `".*"` wildcard suffix. A completely empty result is silently ignored.
fn extract_import(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    let full_text = text_of(source, node);
    let trimmed = full_text.trim();
    if !trimmed.starts_with("import ") {
        return;
    }
    let after_prefix = trimmed["import ".len()..].trim_start();
    // Strip " as Alias" renaming suffix.
    let without_alias = after_prefix
        .split(" as ")
        .next()
        .unwrap_or(after_prefix)
        .trim_end();
    // Strip trailing ".*" wildcard and any residual trailing dot.
    let path = without_alias
        .strip_suffix(".*")
        .unwrap_or(without_alias)
        .trim_end_matches('.');
    if path.is_empty() {
        return;
    }
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: path.to_lowercase(),
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Extracts a top-level `function_declaration` into `result`.
///
/// Emits a function node `B_<fn>` (lowercased via [`decl_name`]) and a `contains` edge from
/// `B` to the node. Top-level function declarations without a resolvable name are silently
/// skipped.
fn extract_function(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(name) = decl_name(node, source) else {
        return;
    };
    let fn_label = format!("{b}_{name}");
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

/// Emits one `inherits` edge applying the local/external split.
///
/// When `base_lower` is in `local_types` (declared in the same file), the source id is
/// `"b_base_lower"` (stem-prefixed). Otherwise the source id is the bare `"base_lower"`
/// (external). The edge runs `base_id → class_label` with relation `"inherits"`.
fn emit_inherits_edge(
    base_lower: &str,
    b: &str,
    class_label: &str,
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
        target: class_label.to_owned(),
        relation: "inherits".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Resolves the outer base-class name from a `user_type` node.
///
/// In tree-sitter-kotlin-ng, a `user_type` contains one or more `simple_user_type` children
/// (for dotted qualified types like `com.example.Base`). Only the **last** `simple_user_type`
/// is consulted to get the leaf class name, ignoring package prefixes. Each `simple_user_type`
/// holds a `type_identifier` or `simple_identifier` as its first named child.
///
/// Falls back to a direct `type_identifier` or `simple_identifier` child of the `user_type`
/// for simpler/flat grammar variants. Returns `None` for unresolvable structures.
fn base_name_from_user_type(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // Strategy A: find the last simple_user_type (handles dotted qualified names).
    let mut last_simple_user_type: Option<tree_sitter::Node<'_>> = None;
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "simple_user_type" {
            last_simple_user_type = Some(child);
        }
    }
    if let Some(sut) = last_simple_user_type {
        for i in 0..sut.named_child_count() {
            let Some(child) = sut.named_child(i) else {
                continue;
            };
            // tree-sitter-kotlin-ng v1.x uses plain "identifier"; older variants use
            // "type_identifier" or "simple_identifier".
            if matches!(child.kind(), "type_identifier" | "simple_identifier" | "identifier") {
                let t = text_of(source, &child);
                if !t.is_empty() {
                    return Some(t.to_lowercase());
                }
            }
        }
    }
    // Strategy B: flat user_type — direct identifier child (all known grammar variants).
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        // tree-sitter-kotlin-ng v1.x uses plain "identifier"; older variants use
        // "type_identifier" or "simple_identifier".
        if matches!(child.kind(), "type_identifier" | "simple_identifier" | "identifier") {
            let t = text_of(source, &child);
            if !t.is_empty() {
                return Some(t.to_lowercase());
            }
        }
    }
    None
}

/// Extracts the base-class name from one `delegation_specifier` node.
///
/// A `delegation_specifier` is one entry in the `:` clause of a Kotlin class header. It is
/// either a `constructor_invocation` (class extended with `()`, e.g. `Animal()`) or a plain
/// `user_type` (interface implemented without call, e.g. `Runnable`). `explicit_delegation`
/// (by-delegation via `by`) is silently skipped — it is not a supertype relationship.
fn extract_one_delegation_specifier(
    spec: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    class_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for i in 0..spec.named_child_count() {
        let Some(child) = spec.named_child(i) else {
            continue;
        };
        match child.kind() {
            "constructor_invocation" => {
                // user_type is the first named child; value_arguments (the call parens) follow.
                if let Some(user_type_node) = child.named_child(0) {
                    if let Some(base) = base_name_from_user_type(&user_type_node, source) {
                        emit_inherits_edge(&base, b, class_label, local_types, result);
                    }
                }
            }
            "user_type" => {
                if let Some(base) = base_name_from_user_type(&child, source) {
                    emit_inherits_edge(&base, b, class_label, local_types, result);
                }
            }
            // "explicit_delegation" (by-delegation) is not a supertype — silently skipped.
            _ => {}
        }
    }
}

/// Extracts `inherits` edges from the delegation-specifier section of a class/object node.
///
/// Walks the named children of `class_node` looking for `delegation_specifier_list` (a wrapper
/// container present in some grammar variants) or bare `delegation_specifier` nodes (in grammar
/// variants that omit the wrapper).
fn extract_class_inheritance(
    class_node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    class_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for i in 0..class_node.named_child_count() {
        let Some(child) = class_node.named_child(i) else {
            continue;
        };
        match child.kind() {
            // tree-sitter-kotlin-ng v1.x uses "delegation_specifiers" (plural) as the
            // container wrapping individual "delegation_specifier" children.
            // Older grammar variants may use "delegation_specifier_list".
            "delegation_specifier_list" | "delegation_specifiers" => {
                for j in 0..child.named_child_count() {
                    let Some(spec) = child.named_child(j) else {
                        continue;
                    };
                    if spec.kind() == "delegation_specifier" {
                        extract_one_delegation_specifier(
                            &spec,
                            source,
                            b,
                            class_label,
                            local_types,
                            result,
                        );
                    }
                }
            }
            "delegation_specifier" => {
                extract_one_delegation_specifier(&child, source, b, class_label, local_types, result);
            }
            _ => {}
        }
    }
}

/// Extracts method and constructor nodes from a `class_body` or `enum_class_body`.
///
/// Walks named children looking for `function_declaration` and `secondary_constructor` nodes.
/// Each qualifying child emits a method node `B_c_m` and a `method` edge from `class_label`.
/// The method name is extracted via [`decl_name`]; secondary constructors receive the fixed
/// name component `"constructor"`. Other children (properties, nested classes, etc.) are
/// silently skipped.
fn extract_class_body_methods(
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
        let raw_name_opt: Option<String> = match child.kind() {
            "function_declaration" => decl_name(&child, source),
            "secondary_constructor" => Some("constructor".to_owned()),
            _ => None,
        };
        let Some(raw) = raw_name_opt else {
            continue;
        };
        let m = method_id(&raw);
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

/// Extracts a `class_declaration` or `interface_declaration` into `result`.
///
/// Emits:
/// - A class/interface node `B_c` and a `contains` edge from `B` to the node.
/// - One `inherits` edge per delegation specifier (`:` clause base types), with local/external
///   split applied.
/// - One method node `B_c_m` and one `method` edge per `function_declaration` or
///   `secondary_constructor` found in the class body.
fn extract_class(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(c) = decl_name(node, source) else {
        return;
    };
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

    extract_class_inheritance(node, source, b, &class_label, local_types, result);

    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "class_body" | "enum_class_body") {
            extract_class_body_methods(&child, source, b, &c, &class_label, source_file, result);
        }
    }
}

/// Extracts an `object_declaration` (Kotlin singleton) into `result`.
///
/// Object declarations are treated as class-like symbols in the graph taxonomy: they emit a
/// node `B_<name>` and a `contains` edge from `B`, plus method nodes for any
/// `function_declaration` children in the object body. Objects can also implement interfaces
/// (via `:` delegation specifiers), so `inherits` edges are emitted for those too.
fn extract_object(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(name) = decl_name(node, source) else {
        return;
    };
    let obj_label = format!("{b}_{name}");

    result.nodes.push(RawNode {
        label: obj_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: obj_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    extract_class_inheritance(node, source, b, &obj_label, local_types, result);

    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() == "class_body" {
            extract_class_body_methods(&child, source, b, &name, &obj_label, source_file, result);
        }
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for KotlinExtractor {
    fn language(&self) -> &'static str {
        "kotlin"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }

    /// Extracts Kotlin nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased); class,
    /// interface, and object nodes `B_<name>`; function nodes `B_<fn>`; method nodes
    /// `B_<cls>_<method>`; with `contains`, `method`, `inherits`, and `imports_from` edges.
    /// `calls`/`uses` are deliberately omitted.
    ///
    /// Both `.kt` and `.kts` extensions are handled identically.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Kotlin grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-kotlin-ng`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid Kotlin syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
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

        // Pass 2: walk top-level source_file children.
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            match child.kind() {
                // tree-sitter-kotlin-ng v1.x uses "import"; older grammar variants use
                // "import_header". Both produce the same full text so extract_import works
                // identically for both.
                "import" | "import_header" => {
                    extract_import(&child, source, &b, &mut result);
                }
                // Some grammar variants wrap imports in a list container node.
                "import_list" => {
                    for j in 0..child.named_child_count() {
                        let Some(hdr) = child.named_child(j) else {
                            continue;
                        };
                        if matches!(hdr.kind(), "import" | "import_header") {
                            extract_import(&hdr, source, &b, &mut result);
                        }
                    }
                }
                "class_declaration" | "interface_declaration" => {
                    extract_class(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "object_declaration" => {
                    extract_object(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "function_declaration" => {
                    extract_function(&child, source, &b, &source_file, &mut result);
                }
                // package_header, file_annotation, type_alias, property_declaration, etc. are
                // silently skipped — not in the graphify taxonomy for Kotlin.
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

    use super::KotlinExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on any error.
    fn extract(src: &str, filename: &str) -> Extraction {
        KotlinExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("kotlin extractor failed on {filename}: {e}"))
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

    /// Returns the node with the given label, panicking with a helpful message if absent.
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
    fn empty_source_yields_exactly_one_file_node_and_no_edges() {
        let ex = extract("", "main.kt");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "main");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn whitespace_only_source_yields_file_node() {
        let ex = extract("   \n\n\t  \n", "utils.kt");
        assert_eq!(ex.nodes.len(), 1, "whitespace-only source must yield 1 node");
        assert_eq!(ex.nodes[0].label, "utils");
    }

    #[test]
    fn malformed_source_handled_gracefully_tree_sitter_is_error_tolerant() {
        // tree-sitter is error-tolerant and always returns a partial tree.
        let ex = extract("@@@@@ NOT VALID KOTLIN !!!", "broken.kt");
        assert!(has_node(&ex, "broken"), "file node must always be present");
    }

    #[test]
    fn package_only_source_yields_file_node_and_no_symbol_nodes() {
        let ex = extract("package com.example\n", "pkg.kt");
        assert_eq!(ex.nodes.len(), 1, "package-only source: 1 node expected");
        assert_eq!(ex.nodes[0].label, "pkg");
        assert_eq!(ex.edges.len(), 0, "package-only source: no edges expected");
    }

    // ── B. File-stem and metadata ──────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HttpClient.kt");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_all_caps_is_fully_lowercased() {
        let ex = extract("", "APIUTILS.kt");
        assert_eq!(ex.nodes[0].label, "apiutils");
    }

    #[test]
    fn kts_extension_is_registered() {
        assert!(
            KotlinExtractor.extensions().contains(&"kts"),
            "kts must be a registered extension"
        );
    }

    #[test]
    fn kt_extension_is_registered() {
        assert!(
            KotlinExtractor.extensions().contains(&"kt"),
            "kt must be a registered extension"
        );
    }

    #[test]
    fn language_slug_is_kotlin() {
        assert_eq!(KotlinExtractor.language(), "kotlin");
    }

    #[test]
    fn kts_file_produces_file_node() {
        let ex = extract("fun main() {}", "script.kts");
        assert!(has_node(&ex, "script"), "kts file node missing");
    }

    // ── C. Top-level functions ─────────────────────────────────────────────────────────────────

    #[test]
    fn function_declaration_emits_fn_node_and_contains_edge() {
        let ex = extract("fun hello() {}", "utils.kt");
        assert!(has_node(&ex, "utils_hello"), "fn node missing");
        assert!(
            has_edge(&ex, "utils", "utils_hello", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased() {
        let ex = extract("fun ParseXML() {}", "p.kt");
        assert!(
            has_node(&ex, "p_parsexml"),
            "function name must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_top_level_functions_all_emitted() {
        let ex = extract("fun a() {}\nfun b() {}\nfun c() {}", "funcs.kt");
        assert!(has_node(&ex, "funcs_a"), "funcs_a missing");
        assert!(has_node(&ex, "funcs_b"), "funcs_b missing");
        assert!(has_node(&ex, "funcs_c"), "funcs_c missing");
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "funcs").collect();
        assert_eq!(fn_nodes.len(), 3, "expected 3 function nodes");
    }

    #[test]
    fn function_span_is_well_formed_and_non_empty() {
        let ex = extract("fun compute(): Int { return 42 }", "math.kt");
        let n = node(&ex, "math_compute");
        assert!(n.span.is_well_formed(), "fn span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "fn span must not be empty: {n:?}");
    }

    #[test]
    fn function_on_third_line_has_correct_start_line() {
        let src = "\n\nfun late() {}";
        let ex = extract(src, "sl.kt");
        let n = node(&ex, "sl_late");
        assert_eq!(n.span.start_line, 3, "fn on line 3 must have start_line=3; got {n:?}");
    }

    // ── D. Class declarations ──────────────────────────────────────────────────────────────────

    #[test]
    fn class_declaration_emits_node_and_contains_edge() {
        let ex = extract("class Foo {}", "models.kt");
        assert!(has_node(&ex, "models_foo"), "class node missing");
        assert!(
            has_edge(&ex, "models", "models_foo", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_lowercased() {
        let ex = extract("class HTTPClient {}", "client.kt");
        assert!(has_node(&ex, "client_httpclient"), "class label must be lowercased");
    }

    #[test]
    fn multiple_classes_all_emitted() {
        let ex = extract("class A {}\nclass B {}\nclass C {}", "classes.kt");
        assert!(has_node(&ex, "classes_a"));
        assert!(has_node(&ex, "classes_b"));
        assert!(has_node(&ex, "classes_c"));
        let contains: Vec<_> = ex.edges.iter().filter(|e| e.relation == "contains").collect();
        assert_eq!(contains.len(), 3, "expected 3 contains edges for 3 classes");
    }

    #[test]
    fn class_with_empty_body_emits_only_class_node_and_contains_edge() {
        let ex = extract("class Empty {}", "e.kt");
        // file node + class node = 2 nodes
        assert_eq!(ex.nodes.len(), 2, "empty class must produce exactly 2 nodes");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 0, "empty class must produce 0 method edges");
    }

    #[test]
    fn abstract_class_is_treated_as_class_declaration() {
        // abstract class uses the same class_declaration node kind in kotlin-ng grammar
        let ex = extract("abstract class Shape {}", "shapes.kt");
        assert!(has_node(&ex, "shapes_shape"), "abstract class node missing");
        assert!(has_edge(&ex, "shapes", "shapes_shape", "contains"));
    }

    #[test]
    fn data_class_is_treated_as_class_declaration() {
        let ex = extract("data class Point(val x: Int, val y: Int)", "point.kt");
        assert!(has_node(&ex, "point_point"), "data class node missing");
        assert!(has_edge(&ex, "point", "point_point", "contains"));
    }

    #[test]
    fn sealed_class_is_treated_as_class_declaration() {
        let ex = extract("sealed class Result {}", "result.kt");
        assert!(has_node(&ex, "result_result"), "sealed class node missing");
    }

    // ── E. Interface declarations ──────────────────────────────────────────────────────────────

    #[test]
    fn interface_emits_node_and_contains_edge() {
        let ex = extract("interface Runnable { fun run() }", "iface.kt");
        assert!(has_node(&ex, "iface_runnable"), "interface node missing");
        assert!(
            has_edge(&ex, "iface", "iface_runnable", "contains"),
            "contains edge missing for interface"
        );
    }

    #[test]
    fn interface_name_is_lowercased() {
        let ex = extract("interface HTTPHandler {}", "handler.kt");
        assert!(
            has_node(&ex, "handler_httphandler"),
            "interface label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_interfaces_all_emitted() {
        // Grammar requires multiline interface bodies; two single-line interfaces on separate
        // lines produce an ERROR parse tree in tree-sitter-kotlin-ng v1.x.
        let src = concat!(
            "interface Runnable {\n    fun run()\n}\n",
            "interface Closeable {\n    fun close()\n}\n",
        );
        let ex = extract(src, "ifaces.kt");
        assert!(has_node(&ex, "ifaces_runnable"), "Runnable missing");
        assert!(has_node(&ex, "ifaces_closeable"), "Closeable missing");
    }

    // ── F. Object declarations ─────────────────────────────────────────────────────────────────

    #[test]
    fn object_declaration_emits_node_and_contains_edge() {
        let ex = extract("object Singleton { fun get() = this }", "singleton.kt");
        assert!(has_node(&ex, "singleton_singleton"), "object node missing");
        assert!(
            has_edge(&ex, "singleton", "singleton_singleton", "contains"),
            "contains edge missing for object"
        );
    }

    #[test]
    fn object_name_is_lowercased() {
        let ex = extract("object MyObject {}", "obj.kt");
        assert!(
            has_node(&ex, "obj_myobject"),
            "object label must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn object_with_method_emits_method_node_and_method_edge() {
        let ex = extract("object Cache { fun get(key: String) = key }", "cache.kt");
        assert!(has_node(&ex, "cache_cache_get"), "object method node missing");
        assert!(has_edge(&ex, "cache_cache", "cache_cache_get", "method"));
    }

    // ── G. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_emits_method_node_and_method_edge() {
        let ex = extract("class Dog { fun bark() {} }", "dog.kt");
        assert!(has_node(&ex, "dog_dog_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_dog", "dog_dog_bark", "method"),
            "method edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn method_name_is_lowercased() {
        let ex = extract("class Foo { fun MyMethod() {} }", "m.kt");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_methods_in_class_all_emitted() {
        // Grammar requires multiline class body for multiple methods; inline
        // multi-method classes produce an ERROR parse tree in tree-sitter-kotlin-ng v1.x.
        let src = "class Foo {\n    fun a() {}\n    fun b() {}\n    fun c() {}\n}";
        let ex = extract(src, "multi.kt");
        assert!(has_node(&ex, "multi_foo_a"), "method a missing");
        assert!(has_node(&ex, "multi_foo_b"), "method b missing");
        assert!(has_node(&ex, "multi_foo_c"), "method c missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn method_label_format_is_b_underscore_cls_underscore_method() {
        let ex = extract("class MyClass { fun doWork() {} }", "mymod.kt");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method label must match b_cls_method format; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let ex = extract("class Cat { fun meow() {} }", "pet.kt");
        assert!(has_edge(&ex, "pet_cat", "pet_cat_meow", "method"));
        assert!(
            !has_edge(&ex, "pet", "pet_cat_meow", "method"),
            "method edge source must be class label, not file label"
        );
    }

    #[test]
    fn underscore_prefix_in_method_name_is_stripped() {
        let ex = extract("class Foo { fun _private() {} }", "priv.kt");
        // method_id("_private") → "private"
        assert!(
            has_node(&ex, "priv_foo_private"),
            "leading underscore must be stripped from method id; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_classes_two_methods_each_produce_correct_totals() {
        // Grammar requires multiline class bodies for multiple methods; inline
        // multi-method classes produce an ERROR parse tree in tree-sitter-kotlin-ng v1.x.
        let src = concat!(
            "class A {\n    fun x() {}\n    fun y() {}\n}\n",
            "class B {\n    fun p() {}\n    fun q() {}\n}\n"
        );
        let ex = extract(src, "ab.kt");
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

    // ── H. Inheritance / delegation specifiers ─────────────────────────────────────────────────

    #[test]
    fn class_extending_external_class_emits_bare_inherits_edge() {
        let ex = extract("class Dog : Animal() {}", "m.kt");
        // "Animal" is external → bare lowercase id
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external extends must yield bare base id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_extending_local_class_emits_stem_prefixed_inherits_edge() {
        let ex = extract("open class Animal {}\nclass Dog : Animal() {}", "m.kt");
        assert!(
            has_edge(&ex, "m_animal", "m_dog", "inherits"),
            "local extends must yield stem-prefixed base id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_implementing_external_interface_emits_inherits_edge() {
        let ex = extract("class Dog : Runnable {}", "m.kt");
        assert!(
            has_edge(&ex, "runnable", "m_dog", "inherits"),
            "external interface implementation must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_with_no_supertypes_produces_no_inherits_edges() {
        let ex = extract("class Standalone {}", "s.kt");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "class with no supertypes must produce 0 inherits edges"
        );
    }

    #[test]
    fn local_and_external_bases_classified_correctly() {
        let src = "open class Base {}\nclass Child : Base(), Serializable {}";
        let ex = extract(src, "mix.kt");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id; edges: {:?}",
            ex.edges
        );
        assert!(
            has_edge(&ex, "serializable", "mix_child", "inherits"),
            "external base must use bare id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_implementing_local_interface_uses_stem_prefixed_base_id() {
        let src = "interface Runnable { fun run() }\nclass Worker : Runnable {}";
        let ex = extract(src, "w.kt");
        assert!(
            has_edge(&ex, "w_runnable", "w_worker", "inherits"),
            "local interface must be stem-prefixed; edges: {:?}",
            ex.edges
        );
    }

    // ── I. Import statements ───────────────────────────────────────────────────────────────────

    #[test]
    fn import_header_emits_imports_from_edge() {
        let ex = extract("import com.example.MyClass", "c.kt");
        assert!(
            has_edge(&ex, "c", "com.example.myclass", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_path_is_lowercased() {
        let ex = extract("import com.Example.HTTPUtils", "c.kt");
        assert!(
            has_edge(&ex, "c", "com.example.httputils", "imports_from"),
            "import path must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_strips_leading_import_keyword() {
        let ex = extract("import kotlin.collections.List", "f.kt");
        // target must not start with "import"
        for edge in &ex.edges {
            if edge.relation == "imports_from" {
                assert!(
                    !edge.target.starts_with("import"),
                    "import keyword must be stripped from target; got {edge:?}"
                );
            }
        }
    }

    #[test]
    fn wildcard_import_strips_star_suffix() {
        let ex = extract("import kotlin.collections.*", "wild.kt");
        assert!(
            has_edge(&ex, "wild", "kotlin.collections", "imports_from"),
            "wildcard import: .* must be stripped; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn aliased_import_strips_as_alias() {
        let ex = extract("import kotlin.collections.List as KList", "alias.kt");
        assert!(
            has_edge(&ex, "alias", "kotlin.collections.list", "imports_from"),
            "aliased import: ' as Alias' must be stripped; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_imports_all_emit_imports_from_edges() {
        let src = "import com.alpha.A\nimport com.beta.B\nimport com.gamma.C\n";
        let ex = extract(src, "multi.kt");
        assert!(has_edge(&ex, "multi", "com.alpha.a", "imports_from"), "A missing");
        assert!(has_edge(&ex, "multi", "com.beta.b", "imports_from"), "B missing");
        assert!(has_edge(&ex, "multi", "com.gamma.c", "imports_from"), "C missing");
        let import_edges = ex.edges.iter().filter(|e| e.relation == "imports_from").count();
        assert_eq!(import_edges, 3, "expected 3 imports_from edges");
    }

    #[test]
    fn import_of_stdlib_kotlin_emits_edge() {
        let ex = extract("import kotlin.math.sqrt", "m.kt");
        assert!(has_edge(&ex, "m", "kotlin.math.sqrt", "imports_from"));
    }

    // ── J. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = concat!(
            "fun foo() { bar() }\n",
            "fun bar() {}\n",
            "class A { fun m() { foo() } }\n",
        );
        let ex = extract(src, "neg.kt");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "val x = 1\nfun useX() = x + 1";
        let ex = extract(src, "neg2.kt");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn method_nodes_are_not_accessible_via_contains_edge() {
        let ex = extract("class Foo { fun go() {} }", "f.kt");
        let method_via_contains = ex
            .edges
            .iter()
            .any(|e| e.relation == "contains" && e.target == "f_foo_go");
        assert!(
            !method_via_contains,
            "method node must NOT appear via a 'contains' edge"
        );
        assert!(
            has_edge(&ex, "f_foo", "f_foo_go", "method"),
            "method node must appear via a 'method' edge"
        );
    }

    // ── K. Edge confidence ─────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "import com.example.X\n",
            "open class Base {}\n",
            "class Derived : Base() { fun work() {} }\n",
            "fun toplevel() {}\n",
        );
        let ex = extract(src, "conf.kt");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Extracted confidence; got {edge:?}"
            );
        }
    }

    // ── L. Span properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let ex = extract("class Foo {}", "span.kt");
        let file_node = node(&ex, "span");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let ex = extract("class Foo {}", "x.kt");
        let n = node(&ex, "x_foo");
        assert_eq!(n.span.start_line, 1, "class on line 1 must have start_line=1; got {n:?}");
    }

    #[test]
    fn class_on_third_line_has_correct_start_line() {
        let src = "\n\nclass Late {}";
        let ex = extract(src, "y.kt");
        let n = node(&ex, "y_late");
        assert_eq!(n.span.start_line, 3, "class on line 3 must have start_line=3; got {n:?}");
    }

    #[test]
    fn all_node_spans_are_well_formed_for_non_trivial_source() {
        let src = "class A { fun run() {} }\nfun b() { val x = 1 }";
        let ex = extract(src, "ws.kt");
        for n in &ex.nodes {
            assert!(
                n.span.is_well_formed(),
                "span must be well-formed for {}: {n:?}",
                n.label
            );
        }
    }

    // ── M. source_file field ───────────────────────────────────────────────────────────────────

    #[test]
    fn source_file_field_present_in_every_node() {
        let src = "class C { fun m() {} }\nfun f() {}";
        let ex = extract(src, "traced.kt");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("traced.kt"),
                "source_file {:?} must contain 'traced.kt'",
                n.source_file
            );
        }
    }

    // ── N. Qualified-id format ─────────────────────────────────────────────────────────────────

    #[test]
    fn class_qualified_id_format_is_b_underscore_classname() {
        let ex = extract("class MyModel {}", "mymod.kt");
        assert!(
            has_node(&ex, "mymod_mymodel"),
            "class qualified id must match b_classname; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn function_qualified_id_format_is_b_underscore_funcname() {
        let ex = extract("fun myFunc() {}", "mymod.kt");
        assert!(
            has_node(&ex, "mymod_myfunc"),
            "function qualified id must match b_funcname; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_qualified_id_format_is_b_underscore_cls_underscore_method() {
        let ex = extract("class MyClass { fun doWork() {} }", "mymod.kt");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method qualified id must match b_cls_method; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── O. Composite / realistic ───────────────────────────────────────────────────────────────

    #[test]
    fn mixed_class_function_import_all_emitted() {
        let src = concat!(
            "import kotlin.math.sqrt\n",
            "class Parser {}\n",
            "fun parse(): String = \"\"\n",
        );
        let ex = extract(src, "mix.kt");
        assert!(has_node(&ex, "mix"), "file node missing");
        assert!(has_node(&ex, "mix_parser"), "class node missing");
        assert!(has_node(&ex, "mix_parse"), "function node missing");
        assert!(has_edge(&ex, "mix", "mix_parser", "contains"));
        assert!(has_edge(&ex, "mix", "mix_parse", "contains"));
        assert!(has_edge(&ex, "mix", "kotlin.math.sqrt", "imports_from"));
    }

    #[test]
    fn class_with_methods_and_inheritance_all_connected() {
        // When `open class Base { fun init() {} }` precedes a second class on the next line
        // the grammar produces an ERROR tree in tree-sitter-kotlin-ng v1.x.
        // Use multiline class bodies so both declarations parse cleanly.
        let src = concat!(
            "open class Base {\n    fun init() {}\n}\n",
            "class Derived : Base() {\n    fun work() {}\n}\n",
        );
        let ex = extract(src, "chain.kt");
        assert!(has_node(&ex, "chain_base"), "base class missing");
        assert!(has_node(&ex, "chain_derived"), "derived class missing");
        assert!(has_node(&ex, "chain_base_init"), "base method missing");
        assert!(has_node(&ex, "chain_derived_work"), "derived method missing");
        assert!(
            has_edge(&ex, "chain_base", "chain_derived", "inherits"),
            "inherits edge missing; edges: {:?}",
            ex.edges
        );
        assert!(has_edge(&ex, "chain_base", "chain_base_init", "method"));
        assert!(has_edge(&ex, "chain_derived", "chain_derived_work", "method"));
    }

    #[test]
    fn realistic_kotlin_file_emits_correct_node_set() {
        let src = concat!(
            "package com.example\n",
            "\n",
            "import kotlin.collections.List\n",
            "\n",
            "interface Repository<T> {\n",
            "    fun findAll(): List<T>\n",
            "}\n",
            "\n",
            "data class User(val id: Int, val name: String)\n",
            "\n",
            "class UserRepository : Repository<User> {\n",
            "    fun findAll(): List<User> = emptyList()\n",
            "    fun save(user: User) {}\n",
            "}\n",
            "\n",
            "fun createRepo(): UserRepository = UserRepository()\n",
        );
        let ex = extract(src, "app.kt");
        assert!(has_node(&ex, "app"), "file node missing");
        assert!(has_node(&ex, "app_repository"), "Repository interface missing");
        assert!(has_node(&ex, "app_user"), "User data class missing");
        assert!(has_node(&ex, "app_userrepository"), "UserRepository missing");
        assert!(has_node(&ex, "app_createrepo"), "createRepo fn missing");
        assert!(has_edge(&ex, "app", "kotlin.collections.list", "imports_from"));
        assert!(has_edge(&ex, "app", "app_repository", "contains"));
        assert!(has_edge(&ex, "app", "app_user", "contains"));
        assert!(has_edge(&ex, "app", "app_userrepository", "contains"));
        assert!(has_edge(&ex, "app", "app_createrepo", "contains"));
    }

    #[test]
    fn object_with_methods_fully_extracted() {
        let src = concat!(
            "object Logger {\n",
            "    fun info(msg: String) {}\n",
            "    fun error(msg: String) {}\n",
            "}\n",
        );
        let ex = extract(src, "log.kt");
        assert!(has_node(&ex, "log_logger"), "Logger object missing");
        assert!(has_node(&ex, "log_logger_info"), "info method missing");
        assert!(has_node(&ex, "log_logger_error"), "error method missing");
        assert!(has_edge(&ex, "log", "log_logger", "contains"));
        assert!(has_edge(&ex, "log_logger", "log_logger_info", "method"));
        assert!(has_edge(&ex, "log_logger", "log_logger_error", "method"));
    }

    #[test]
    fn interface_with_methods_extracts_correctly() {
        let src = concat!(
            "interface Animal {\n",
            "    fun speak(): String\n",
            "    fun move()\n",
            "}\n",
        );
        let ex = extract(src, "animal.kt");
        assert!(has_node(&ex, "animal_animal"), "interface node missing");
        // Note: interface method bodies are abstract — tree-sitter may still parse them as
        // function_declaration children of the class_body, so method nodes may be emitted.
        // We only assert the interface node and contains edge here.
        assert!(has_edge(&ex, "animal", "animal_animal", "contains"));
    }

    #[test]
    fn file_stem_with_underscores_preserved_in_labels() {
        let ex = extract("class Foo {}", "my_module.kt");
        assert!(
            has_node(&ex, "my_module_foo"),
            "underscores in stem must be preserved; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }
}
