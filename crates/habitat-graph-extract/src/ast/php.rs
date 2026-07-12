//! PHP AST extractor (`tree-sitter-php`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class, interface,
//! trait, and enum nodes `B_<name>`; top-level function nodes `B_<fn>`; method nodes
//! `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class/trait/enum→method),
//! `inherits` (extends and implements, with local/external split), `imports_from`
//! (file→namespace use path). `calls` and `uses` are deliberately **NOT** emitted — they are
//! name-resolution heuristics that would not match graphify.
//!
//! Input is expected to be a full PHP file; the `LANGUAGE_PHP` grammar is used (which handles
//! the `<?php` tag). Abstract and `final` modifiers do not affect extraction. Grouped
//! `use App\Models\{A, B};` imports are resolved into individual `imports_from` edges per
//! clause. Aliased imports (`use Foo as F`) emit the unaliased path.
//!
//! Trait `use` clauses inside class bodies are structural (not inheritance) and are not emitted
//! as `inherits` edges. PHP 8.1+ enum methods are extracted identically to class methods.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from PHP source using `tree-sitter-php`, in graphify's
/// qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (lowercased stem): one file node `B`; class, interface, trait,
/// and enum nodes `B_<name>`; top-level function nodes `B_<fn>`; method nodes
/// `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class→method), `inherits`
/// (base→derived, with local/external split for both `extends` and `implements`),
/// `imports_from` (file→namespace use path). `calls`/`uses` are deliberately omitted.
///
/// An empty file (or a file containing only `<?php`) produces exactly one node (the file node)
/// and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct PhpExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Returns the method id component `m` of the qualified label `B_C_m`.
///
/// Strips leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting entirely
/// of underscores such as `___`).
///
/// Examples: `__construct` → `"construct"`, `_private` → `"private"`,
/// `myMethod` → `"mymethod"`, `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Collects the lowercased names of all top-level class, interface, trait, and enum
/// declarations from the root `program` node.
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
            "class_declaration"
                | "interface_declaration"
                | "trait_declaration"
                | "enum_declaration"
        ) {
            if let Some(name_node) = child.child_by_field_name("name") {
                names.insert(text_of(source, &name_node).to_lowercase());
            }
        }
    }
    names
}

/// Resolves a heritage child node (from `base_clause` or `class_interface_clause`) to its
/// lowercased name string.
///
/// Handles:
/// - `name` — plain, unqualified class name (`Controller`).
/// - `qualified_name` — backslash-separated namespace path (`App\Http\Controllers\Controller`).
/// - `relative_name` — starts with a leading `\` (`\App\Http\Controller`); the leading `\`
///   is stripped before lowercasing.
///
/// Returns `None` for any other node kind (silently skipped).
fn heritage_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "name" | "qualified_name" | "relative_name" => {
            let raw = text_of(source, node);
            let trimmed = raw.trim_start_matches('\\');
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_lowercase())
            }
        }
        _ => None,
    }
}

/// Emits an `inherits` edge applying the local/external split.
///
/// A base whose lowercased name is in `local_types` (declared in the same file) gets
/// `base_id = "b_<base>"`, an external base gets `base_id = "<base>"`.
fn emit_inherits(
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

/// Extracts `inherits` edges from a `base_clause` or `class_interface_clause` node.
///
/// Both clause kinds carry the same child type set (`name`, `qualified_name`,
/// `relative_name`). Each resolved child emits one `inherits` edge via [`emit_inherits`].
fn extract_heritage_clause(
    clause: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    target_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for ci in 0..clause.named_child_count() {
        let Some(base_node) = clause.named_child(ci) else {
            continue;
        };
        if let Some(base_lower) = heritage_name(&base_node, source) {
            emit_inherits(&base_lower, b, target_label, local_types, result);
        }
    }
}

/// Extracts `method_declaration` children from a class body node.
///
/// Works on both `declaration_list` (class/interface/trait body) and
/// `enum_declaration_list` (PHP 8.1+ enum body). Emits a method node `B_c_m` and a
/// `method` edge from `class_label` for each `method_declaration` found.
/// The [`method_id`] helper strips leading/trailing `_` before lowercasing.
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
        if child.kind() != "method_declaration" {
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

/// Extracts one `function_definition` into `result`.
///
/// Emits a function node `B_<fn>` (name lowercased) and a `contains` edge from `B`.
fn extract_function(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let fn_lower = text_of(source, &name_node).to_lowercase();
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

/// Extracts one `class_declaration` into `result`.
///
/// Emits:
/// - A class node `B_c` (lowercased class name) and a `contains` edge from `B`.
/// - One `inherits` edge per base in the `base_clause` (PHP `extends`), with local/external
///   split. PHP supports single inheritance only, but the grammar allows arbitrary bases.
/// - One `inherits` edge per type in the `class_interface_clause` (PHP `implements`), with
///   local/external split. PHP supports multiple interface implementation.
/// - One method node `B_c_m` and a `method` edge per `method_declaration` in the class body.
///
/// Abstract and `final` modifiers are silently ignored (they appear as sibling children).
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

    // Walk all named children; dispatch heritage clauses, skip everything else.
    for idx in 0..node.named_child_count() {
        let Some(child) = node.named_child(idx) else {
            continue;
        };
        match child.kind() {
            "base_clause" | "class_interface_clause" => {
                extract_heritage_clause(&child, source, b, &class_label, local_types, result);
            }
            _ => {}
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(&body, source, b, &c, &class_label, source_file, result);
    }
}

/// Extracts one `interface_declaration` into `result`.
///
/// Emits an interface node `B_<iface>` and a `contains` edge from `B`. If the interface
/// declares a `base_clause` (`extends`), one `inherits` edge per parent interface is emitted
/// (PHP interfaces support multi-inheritance). Method signatures in the interface body are
/// extracted as method nodes (useful for default-method patterns in PHP 8+).
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

    for idx in 0..node.named_child_count() {
        let Some(child) = node.named_child(idx) else {
            continue;
        };
        if child.kind() == "base_clause" {
            extract_heritage_clause(&child, source, b, &iface_label, local_types, result);
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(
            &body,
            source,
            b,
            &iface_lower,
            &iface_label,
            source_file,
            result,
        );
    }
}

/// Extracts one `trait_declaration` into `result`.
///
/// Emits a trait node `B_<trait>` and a `contains` edge from `B`. PHP traits do not support
/// `extends`/`implements`, so no `inherits` edges are emitted. Trait `use` clauses inside the
/// body (structural composition) are also not emitted. Method declarations in the trait body
/// are extracted as method nodes.
fn extract_trait(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let trait_lower = text_of(source, &name_node).to_lowercase();
    let trait_label = format!("{b}_{trait_lower}");

    result.nodes.push(RawNode {
        label: trait_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: trait_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(
            &body,
            source,
            b,
            &trait_lower,
            &trait_label,
            source_file,
            result,
        );
    }
}

/// Extracts one `enum_declaration` (PHP 8.1+) into `result`.
///
/// Emits an enum node `B_<enum>` and a `contains` edge from `B`. If the enum declares
/// `class_interface_clause` children (`implements`), one `inherits` edge per type is emitted.
/// Methods in the enum body (`enum_declaration_list`) are extracted as method nodes (PHP 8.1+
/// enums support method declarations).
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

    for idx in 0..node.named_child_count() {
        let Some(child) = node.named_child(idx) else {
            continue;
        };
        if child.kind() == "class_interface_clause" {
            extract_heritage_clause(&child, source, b, &enum_label, local_types, result);
        }
    }

    if let Some(body) = node.child_by_field_name("body") {
        extract_body_methods(
            &body,
            source,
            b,
            &enum_lower,
            &enum_label,
            source_file,
            result,
        );
    }
}

/// Extracts the import path from the first `name` or `qualified_name` child of a
/// `namespace_use_clause`.
///
/// The alias (`as Alias`) is a keyed field (`alias`) and does not appear first among the
/// unkeyed named children, so the path is always the first `name` or `qualified_name`
/// encountered. Leading backslashes are stripped; the result is lowercased.
///
/// Returns `None` if no `name` / `qualified_name` child is found or the result is empty.
fn use_clause_path(clause: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    for i in 0..clause.named_child_count() {
        let Some(child) = clause.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "name" | "qualified_name") {
            let raw = text_of(source, &child);
            let path = raw.trim_start_matches('\\').to_lowercase();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    None
}

/// Extracts one `namespace_use_declaration` into `result`.
///
/// Handles three forms:
/// 1. Simple import: `use App\Http\Controllers\Controller;`
///    — one `namespace_use_clause` child → one `imports_from` edge.
/// 2. Multiple imports: `use A; use B;` (each is a separate declaration) — handled
///    per-declaration by the caller.
/// 3. Grouped import: `use App\Models\{User, Post};`
///    — a `namespace_name` child (the prefix) plus a `body` field (`namespace_use_group`)
///    containing `namespace_use_clause` entries; each clause path is prefixed with the
///    namespace and emits one `imports_from` edge.
///
/// The `type` qualifier (`const` / `function`) does not affect extraction.
fn extract_use_decl(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    // Collect the namespace prefix (present in grouped imports) and emit simple-clause edges.
    let mut prefix: Option<String> = None;

    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "namespace_name" => {
                // Grouped-import prefix: e.g. `App\Models` in `use App\Models\{User, Post};`
                let raw = text_of(source, &child);
                prefix = Some(raw.trim_start_matches('\\').to_lowercase());
            }
            "namespace_use_clause" => {
                if let Some(path) = use_clause_path(&child, source) {
                    result.edges.push(RawEdge {
                        source: b.to_owned(),
                        target: path,
                        relation: "imports_from".to_owned(),
                        confidence: Confidence::Extracted,
                    });
                }
            }
            _ => {}
        }
    }

    // Grouped import body: `{User, Post}` clauses prepended with the prefix.
    if let Some(group) = node.child_by_field_name("body") {
        for i in 0..group.named_child_count() {
            let Some(clause) = group.named_child(i) else {
                continue;
            };
            if clause.kind() != "namespace_use_clause" {
                continue;
            }
            if let Some(local_path) = use_clause_path(&clause, source) {
                let full_path = match &prefix {
                    Some(p) => format!("{p}\\{local_path}"),
                    None => local_path,
                };
                result.edges.push(RawEdge {
                    source: b.to_owned(),
                    target: full_path,
                    relation: "imports_from".to_owned(),
                    confidence: Confidence::Extracted,
                });
            }
        }
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for PhpExtractor {
    fn language(&self) -> &'static str {
        "php"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["php"]
    }

    /// Extracts PHP nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// class, interface, trait, and enum nodes `B_<name>`; top-level function nodes `B_<fn>`;
    /// method nodes `B_<cls>_<method>`; with `contains`, `method`, `inherits`, and
    /// `imports_from` edges. `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file (or a file containing only `<?php`) produces exactly one node (the file
    /// node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The PHP grammar could not be installed on the parser (should not occur with a correctly
    ///   linked `tree-sitter-php`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid PHP syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
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

        // Pass 2: walk top-level `program` children; `statement` is a transparent supertype so
        // concrete kinds appear directly (class_declaration, function_definition, etc.).
        for idx in 0..root.named_child_count() {
            let Some(child) = root.named_child(idx) else {
                continue;
            };
            match child.kind() {
                "function_definition" => {
                    extract_function(&child, source, &b, &source_file, &mut result);
                }
                "class_declaration" => {
                    extract_class(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "interface_declaration" => {
                    extract_interface(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "trait_declaration" => {
                    extract_trait(&child, source, &b, &source_file, &mut result);
                }
                "enum_declaration" => {
                    extract_enum(&child, source, &b, &source_file, &local_types, &mut result);
                }
                "namespace_use_declaration" => {
                    extract_use_decl(&child, source, &b, &mut result);
                }
                // php_tag, text, namespace_definition, declare_statement, etc. — skipped.
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

    use super::PhpExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Runs the extractor on `src` as if it came from `filename`; panics on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        PhpExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("php extractor failed on {filename}: {e}"))
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
                    "node '{label}' not found; got {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    // ── A. Empty / trivial ─────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_no_edges() {
        let ex = extract("", "empty.php");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn empty_php_tag_file_yields_only_file_node() {
        let ex = extract("<?php\n", "minimal.php");
        assert_eq!(ex.nodes.len(), 1, "<?php only yields exactly 1 node");
        assert_eq!(ex.nodes[0].label, "minimal");
        assert_eq!(ex.edges.len(), 0, "<?php only yields no edges");
    }

    #[test]
    fn malformed_source_returns_ok_tree_sitter_error_tolerant() {
        // tree-sitter is error-tolerant: garbled input always produces a (partial) tree.
        let ex = extract("!!!NOT VALID PHP @@@", "broken.php");
        assert!(
            has_node(&ex, "broken"),
            "file node must be present for garbled source"
        );
    }

    // ── B. File stem handling ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("<?php\n", "UserController.php");
        assert_eq!(ex.nodes[0].label, "usercontroller");
    }

    #[test]
    fn file_stem_with_all_caps_fully_lowercased() {
        let ex = extract("<?php\n", "APIHELPER.php");
        assert_eq!(ex.nodes[0].label, "apihelper");
    }

    #[test]
    fn file_stem_preserves_underscores_and_digits() {
        let ex = extract("<?php\n", "my_module2.php");
        assert_eq!(ex.nodes[0].label, "my_module2");
    }

    // ── C. Language / extension ────────────────────────────────────────────────────────────────

    #[test]
    fn language_slug_is_php() {
        assert_eq!(PhpExtractor.language(), "php");
    }

    #[test]
    fn php_extension_is_registered() {
        let exts = PhpExtractor.extensions();
        assert!(exts.contains(&"php"), "php extension must be registered");
    }

    // ── D. Function definitions ────────────────────────────────────────────────────────────────

    #[test]
    fn function_definition_emits_fn_node_and_contains_edge() {
        let src = "<?php\nfunction greet() {}";
        let ex = extract(src, "utils.php");
        assert!(has_node(&ex, "utils_greet"), "function node missing");
        assert!(
            has_edge(&ex, "utils", "utils_greet", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased_in_label() {
        let src = "<?php\nfunction ParseXML() {}";
        let ex = extract(src, "p.php");
        assert!(
            has_node(&ex, "p_parsexml"),
            "function label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_top_level_functions_all_emitted() {
        let src = "<?php\nfunction a() {}\nfunction b() {}\nfunction c() {}";
        let ex = extract(src, "f.php");
        assert!(has_node(&ex, "f_a"));
        assert!(has_node(&ex, "f_b"));
        assert!(has_node(&ex, "f_c"));
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "f").collect();
        assert_eq!(fn_nodes.len(), 3, "expected 3 function nodes");
    }

    #[test]
    fn function_label_format_is_b_underscore_fn() {
        let src = "<?php\nfunction myFunc() {}";
        let ex = extract(src, "mod.php");
        assert!(
            has_node(&ex, "mod_myfunc"),
            "function label must follow B_fn format"
        );
    }

    #[test]
    fn function_span_is_well_formed_and_non_empty() {
        let src = "<?php\nfunction foo() { return 1; }";
        let ex = extract(src, "x.php");
        let n = node(&ex, "x_foo");
        assert!(n.span.is_well_formed(), "span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "span must not be empty: {n:?}");
    }

    // ── E. Class declarations ──────────────────────────────────────────────────────────────────

    #[test]
    fn class_declaration_emits_class_node_and_contains_edge() {
        let src = "<?php\nclass HTTPError {}";
        let ex = extract(src, "errors.php");
        assert!(has_node(&ex, "errors_httperror"), "class node missing");
        assert!(
            has_edge(&ex, "errors", "errors_httperror", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_fully_lowercased_in_label() {
        let src = "<?php\nclass XMLParser {}";
        let ex = extract(src, "parser.php");
        assert!(
            has_node(&ex, "parser_xmlparser"),
            "class label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_top_level_classes_all_emitted() {
        let src = "<?php\nclass A {}\nclass B {}\nclass C {}";
        let ex = extract(src, "classes.php");
        assert!(has_node(&ex, "classes_a"));
        assert!(has_node(&ex, "classes_b"));
        assert!(has_node(&ex, "classes_c"));
    }

    #[test]
    fn class_with_no_heritage_produces_no_inherits_edges() {
        let src = "<?php\nclass Standalone {}";
        let ex = extract(src, "s.php");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "class with no heritage must not produce inherits edges"
        );
    }

    #[test]
    fn abstract_class_emits_class_node_and_contains_edge() {
        let src = "<?php\nabstract class Shape {}";
        let ex = extract(src, "shapes.php");
        assert!(has_node(&ex, "shapes_shape"), "abstract class node missing");
        assert!(
            has_edge(&ex, "shapes", "shapes_shape", "contains"),
            "contains edge missing for abstract class"
        );
    }

    // ── F. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_emits_method_node_and_method_edge() {
        let src = "<?php\nclass Foo { public function bark() {} }";
        let ex = extract(src, "dog.php");
        assert!(has_node(&ex, "dog_foo_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_foo", "dog_foo_bark", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn method_name_is_lowercased_in_label() {
        let src = "<?php\nclass Foo { public function myMethod() {} }";
        let ex = extract(src, "m.php");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_methods_in_class_all_emitted() {
        let src =
            "<?php\nclass Foo { public function alpha() {} public function beta() {} public function gamma() {} }";
        let ex = extract(src, "methods.php");
        assert!(has_node(&ex, "methods_foo_alpha"));
        assert!(has_node(&ex, "methods_foo_beta"));
        assert!(has_node(&ex, "methods_foo_gamma"));
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn method_label_format_is_b_underscore_cls_underscore_method() {
        let src = "<?php\nclass MyClass { public function doWork() {} }";
        let ex = extract(src, "mymod.php");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method label must match B_cls_method format"
        );
    }

    #[test]
    fn underscore_prefix_stripped_from_method_id() {
        let src = "<?php\nclass Foo { private function _private() {} }";
        let ex = extract(src, "mod.php");
        assert!(
            has_node(&ex, "mod_foo_private"),
            "leading underscore must be stripped from method id; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn double_underscore_magic_method_stripped_correctly() {
        let src = "<?php\nclass Widget { public function __construct() {} }";
        let ex = extract(src, "w.php");
        assert!(
            has_node(&ex, "w_widget_construct"),
            "__construct must be stripped to 'construct'; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let src = "<?php\nclass Dog { public function run() {} }";
        let ex = extract(src, "pet.php");
        assert!(has_edge(&ex, "pet_dog", "pet_dog_run", "method"));
        assert!(
            !has_edge(&ex, "pet", "pet_dog_run", "method"),
            "method edge source must be the class label, not the file label"
        );
    }

    // ── G. Inheritance — extends ───────────────────────────────────────────────────────────────

    #[test]
    fn extends_external_class_emits_inherits_edge_with_bare_base_id() {
        let src = "<?php\nclass Dog extends Animal {}";
        let ex = extract(src, "m.php");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external extends must yield bare lowercased base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_local_class_emits_inherits_edge_with_stem_prefixed_base_id() {
        let src = "<?php\nclass A {}\nclass B extends A {}";
        let ex = extract(src, "m.php");
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local extends must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_qualified_class_emits_external_inherits_edge() {
        // Qualified names are always external (they have namespace prefix).
        let src = r"<?php
use App\Http\Controller;
class UserCtrl extends Controller {}";
        let ex = extract(src, "ctrl.php");
        // "controller" is from a simple name via base_clause; not in local_types
        assert!(
            has_edge(&ex, "controller", "ctrl_userctrl", "inherits"),
            "simple name from extends must emit external inherits edge"
        );
    }

    #[test]
    fn local_and_external_bases_mixed_correctly_classified() {
        let src = "<?php\nclass Base {}\nclass Child extends Base {}";
        let ex = extract(src, "mix.php");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
    }

    // ── H. Inheritance — implements ───────────────────────────────────────────────────────────

    #[test]
    fn implements_external_interface_emits_inherits_edge() {
        let src = "<?php\nclass Dog implements Runnable {}";
        let ex = extract(src, "m.php");
        assert!(
            has_edge(&ex, "runnable", "m_dog", "inherits"),
            "external implements must emit inherits edge"
        );
    }

    #[test]
    fn implements_local_interface_emits_inherits_edge_with_stem_prefix() {
        let src = "<?php\ninterface Runnable {}\nclass Dog implements Runnable {}";
        let ex = extract(src, "m.php");
        assert!(
            has_edge(&ex, "m_runnable", "m_dog", "inherits"),
            "local implements must use stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_implements_all_emit_inherits_edges() {
        let src = "<?php\nclass Foo implements Bar, Baz, Qux {}";
        let ex = extract(src, "m.php");
        assert!(has_edge(&ex, "bar", "m_foo", "inherits"));
        assert!(has_edge(&ex, "baz", "m_foo", "inherits"));
        assert!(has_edge(&ex, "qux", "m_foo", "inherits"));
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(
            inherits, 3,
            "expected 3 inherits edges from multiple implements"
        );
    }

    #[test]
    fn both_extends_and_implements_emit_both_inherits_edges() {
        let src = "<?php\nclass Dog extends Animal implements Runnable {}";
        let ex = extract(src, "m.php");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "extends edge missing"
        );
        assert!(
            has_edge(&ex, "runnable", "m_dog", "inherits"),
            "implements edge missing"
        );
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        assert_eq!(inherits.len(), 2, "expected exactly 2 inherits edges");
    }

    // ── I. Interface declarations ──────────────────────────────────────────────────────────────

    #[test]
    fn interface_declaration_emits_iface_node_and_contains_edge() {
        let src = "<?php\ninterface User { public function getName(): string; }";
        let ex = extract(src, "models.php");
        assert!(has_node(&ex, "models_user"), "interface node missing");
        assert!(
            has_edge(&ex, "models", "models_user", "contains"),
            "contains edge missing for interface"
        );
    }

    #[test]
    fn interface_name_is_lowercased_in_label() {
        let src = "<?php\ninterface HTTPClient {}";
        let ex = extract(src, "iface.php");
        assert!(
            has_node(&ex, "iface_httpclient"),
            "interface label must be lowercased"
        );
    }

    #[test]
    fn interface_extends_emits_inherits_edge() {
        let src = "<?php\ninterface Countable extends Traversable {}";
        let ex = extract(src, "iface.php");
        assert!(
            has_edge(&ex, "traversable", "iface_countable", "inherits"),
            "interface extends must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_interfaces_all_emitted() {
        let src = "<?php\ninterface A {}\ninterface B {}\ninterface C {}";
        let ex = extract(src, "ifaces.php");
        assert!(has_node(&ex, "ifaces_a"));
        assert!(has_node(&ex, "ifaces_b"));
        assert!(has_node(&ex, "ifaces_c"));
        let contains = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(contains, 3, "expected 3 contains edges");
    }

    // ── J. Trait declarations ──────────────────────────────────────────────────────────────────

    #[test]
    fn trait_declaration_emits_trait_node_and_contains_edge() {
        let src = "<?php\ntrait Loggable { public function log() {} }";
        let ex = extract(src, "traits.php");
        assert!(has_node(&ex, "traits_loggable"), "trait node missing");
        assert!(
            has_edge(&ex, "traits", "traits_loggable", "contains"),
            "contains edge missing for trait"
        );
    }

    #[test]
    fn trait_name_is_lowercased_in_label() {
        let src = "<?php\ntrait HTTPCacheable {}";
        let ex = extract(src, "t.php");
        assert!(
            has_node(&ex, "t_httpcacheable"),
            "trait label must be lowercased"
        );
    }

    #[test]
    fn trait_methods_are_extracted_as_method_nodes() {
        let src = "<?php\ntrait Serializable { public function serialize() {} public function unserialize() {} }";
        let ex = extract(src, "tr.php");
        assert!(has_node(&ex, "tr_serializable_serialize"));
        assert!(has_node(&ex, "tr_serializable_unserialize"));
        assert!(has_edge(
            &ex,
            "tr_serializable",
            "tr_serializable_serialize",
            "method"
        ));
    }

    // ── K. Enum declarations ───────────────────────────────────────────────────────────────────

    #[test]
    fn enum_declaration_emits_enum_node_and_contains_edge() {
        let src = "<?php\nenum Status { case Active; case Inactive; }";
        let ex = extract(src, "enums.php");
        assert!(has_node(&ex, "enums_status"), "enum node missing");
        assert!(
            has_edge(&ex, "enums", "enums_status", "contains"),
            "contains edge missing for enum"
        );
    }

    #[test]
    fn enum_name_is_lowercased_in_label() {
        let src = "<?php\nenum HTTPMethod: string { case GET = 'GET'; case POST = 'POST'; }";
        let ex = extract(src, "http.php");
        assert!(
            has_node(&ex, "http_httpmethod"),
            "enum label must be lowercased"
        );
    }

    #[test]
    fn enum_with_implements_emits_inherits_edge() {
        let src = "<?php\nenum Status implements HasLabel { case Active; }";
        let ex = extract(src, "e.php");
        assert!(
            has_edge(&ex, "haslabel", "e_status", "inherits"),
            "enum implements must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    // ── L. Use declarations (imports) ─────────────────────────────────────────────────────────

    #[test]
    fn simple_use_emits_imports_from_edge() {
        let src = "<?php\nuse Foo;";
        let ex = extract(src, "mod.php");
        assert!(
            has_edge(&ex, "mod", "foo", "imports_from"),
            "simple use must emit imports_from edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn qualified_use_emits_imports_from_edge() {
        let src = r"<?php
use App\Http\Controllers\Controller;";
        let ex = extract(src, "mod.php");
        assert!(
            has_edge(
                &ex,
                "mod",
                "app\\http\\controllers\\controller",
                "imports_from"
            ),
            "qualified use must emit backslash-separated path; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_use_declarations_all_emitted() {
        let src = r"<?php
use App\Models\User;
use App\Models\Post;
use App\Http\Request;";
        let ex = extract(src, "ctrl.php");
        let import_edges: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .collect();
        assert_eq!(import_edges.len(), 3, "expected 3 imports_from edges");
        assert!(has_edge(&ex, "ctrl", "app\\models\\user", "imports_from"));
        assert!(has_edge(&ex, "ctrl", "app\\models\\post", "imports_from"));
        assert!(has_edge(&ex, "ctrl", "app\\http\\request", "imports_from"));
    }

    #[test]
    fn use_with_alias_emits_unaliased_path() {
        // `use App\Models\User as U;` — we emit the path "app\models\user", not the alias.
        let src = r"<?php
use App\Models\User as U;";
        let ex = extract(src, "mod.php");
        assert!(
            has_edge(&ex, "mod", "app\\models\\user", "imports_from"),
            "aliased use must emit the original path, not the alias; edges: {:?}",
            ex.edges
        );
        // No edge with "u" as target.
        assert!(
            !has_edge(&ex, "mod", "u", "imports_from"),
            "alias name must NOT appear as the import target"
        );
    }

    // ── M. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_emitted() {
        let src = "<?php\nclass Foo { public function bar() { $this->baz(); } }";
        let ex = extract(src, "x.php");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "calls").count(),
            0,
            "calls edges must never be emitted"
        );
    }

    #[test]
    fn no_uses_edges_emitted() {
        let src = "<?php\nclass Foo { public function bar($x) { return $x + 1; } }";
        let ex = extract(src, "x.php");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "uses").count(),
            0,
            "uses edges must never be emitted"
        );
    }

    // ── N. Confidence and structural properties ────────────────────────────────────────────────

    #[test]
    fn all_edges_have_extracted_confidence() {
        let src = r"<?php
use Foo\Bar;
class A extends B implements C {
    public function doWork() {}
}";
        let ex = extract(src, "conf.php");
        for e in &ex.edges {
            assert_eq!(
                e.confidence,
                Confidence::Extracted,
                "all PHP edges must have Extracted confidence; got {:?} on edge {e:?}",
                e.confidence
            );
        }
    }

    #[test]
    fn all_nodes_have_matching_source_file() {
        let src = "<?php\nclass Foo { public function bar() {} }";
        let ex = extract(src, "myfile.php");
        for n in &ex.nodes {
            assert_eq!(
                n.source_file, "myfile.php",
                "every node must carry the source file path; offending node: {n:?}"
            );
        }
    }

    #[test]
    fn class_qualified_id_format_is_b_classname() {
        let src = "<?php\nclass MyClass {}";
        let ex = extract(src, "mymod.php");
        assert!(
            has_node(&ex, "mymod_myclass"),
            "class qualified-id must follow B_ClassName format"
        );
    }

    #[test]
    fn method_qualified_id_format_is_b_cls_method() {
        let src = "<?php\nclass MyClass { public function myMethod() {} }";
        let ex = extract(src, "mymod.php");
        assert!(
            has_node(&ex, "mymod_myclass_mymethod"),
            "method qualified-id must follow B_Cls_Method format"
        );
    }

    // ── O. Span checks ────────────────────────────────────────────────────────────────────────

    #[test]
    fn class_span_is_well_formed_and_non_empty() {
        let src = "<?php\nclass Foo { public function bar() {} }";
        let ex = extract(src, "x.php");
        let n = node(&ex, "x_foo");
        assert!(
            n.span.is_well_formed(),
            "class span must be well-formed: {n:?}"
        );
        assert!(!n.span.is_empty(), "class span must not be empty: {n:?}");
    }

    #[test]
    fn method_span_is_well_formed_and_on_correct_line() {
        let src = "<?php\n\nclass Foo {\n    public function bar() {}\n}";
        let ex = extract(src, "x.php");
        let n = node(&ex, "x_foo_bar");
        assert!(
            n.span.is_well_formed(),
            "method span must be well-formed: {n:?}"
        );
        // Method is on line 4 (1-indexed, 1 for <?php, 1 blank, 1 class, 1 method)
        assert!(
            n.span.start_line >= 3,
            "method on line 4 must have start_line>=3; got {n:?}"
        );
    }

    // ── P. Composite / realistic tests ────────────────────────────────────────────────────────

    #[test]
    fn realistic_laravel_controller_class() {
        let src = r"<?php
namespace App\Http\Controllers;

use App\Models\User;
use Illuminate\Http\Request;
use Illuminate\Routing\Controller;

class UserController extends Controller {
    public function __construct() {}
    public function index() {}
    public function show(Request $request, $id) {}
    public function store(Request $request) {}
}";
        let ex = extract(src, "UserController.php");
        // File node
        assert!(has_node(&ex, "usercontroller"));
        // Class node
        assert!(has_node(&ex, "usercontroller_usercontroller"));
        assert!(has_edge(
            &ex,
            "usercontroller",
            "usercontroller_usercontroller",
            "contains"
        ));
        // Heritage: extends Controller (external)
        assert!(has_edge(
            &ex,
            "controller",
            "usercontroller_usercontroller",
            "inherits"
        ));
        // Methods (constructor stripped to "construct")
        assert!(has_node(&ex, "usercontroller_usercontroller_construct"));
        assert!(has_node(&ex, "usercontroller_usercontroller_index"));
        assert!(has_node(&ex, "usercontroller_usercontroller_show"));
        assert!(has_node(&ex, "usercontroller_usercontroller_store"));
        // Imports
        assert!(has_edge(
            &ex,
            "usercontroller",
            "app\\models\\user",
            "imports_from"
        ));
        assert!(has_edge(
            &ex,
            "usercontroller",
            "illuminate\\http\\request",
            "imports_from"
        ));
        assert!(has_edge(
            &ex,
            "usercontroller",
            "illuminate\\routing\\controller",
            "imports_from"
        ));
    }

    #[test]
    fn full_php_file_with_multiple_symbol_types() {
        let src = r"<?php

use Psr\Log\LoggerInterface;

function helper_fn() {}

interface Greetable {
    public function greet(): string;
}

trait Timestampable {
    public function getCreatedAt() {}
}

class GreeterService implements Greetable {
    use Timestampable;
    public function greet(): string { return 'hello'; }
    public function _internal() {}
}";
        let ex = extract(src, "service.php");

        // File node
        assert!(has_node(&ex, "service"));

        // Function
        assert!(has_node(&ex, "service_helper_fn"));
        assert!(has_edge(&ex, "service", "service_helper_fn", "contains"));

        // Interface
        assert!(has_node(&ex, "service_greetable"));
        assert!(has_edge(&ex, "service", "service_greetable", "contains"));
        // Interface method
        assert!(has_node(&ex, "service_greetable_greet"));

        // Trait
        assert!(has_node(&ex, "service_timestampable"));
        assert!(has_edge(
            &ex,
            "service",
            "service_timestampable",
            "contains"
        ));
        assert!(has_node(&ex, "service_timestampable_getcreatedat"));

        // Class
        assert!(has_node(&ex, "service_greeterservice"));
        assert!(has_edge(
            &ex,
            "service",
            "service_greeterservice",
            "contains"
        ));
        // implements Greetable (local)
        assert!(has_edge(
            &ex,
            "service_greetable",
            "service_greeterservice",
            "inherits"
        ));
        // Methods
        assert!(has_node(&ex, "service_greeterservice_greet"));
        // _internal → "internal" (stripped _)
        assert!(has_node(&ex, "service_greeterservice_internal"));

        // Import
        assert!(has_edge(
            &ex,
            "service",
            "psr\\log\\loggerinterface",
            "imports_from"
        ));

        // No calls or uses edges
        assert_eq!(ex.edges.iter().filter(|e| e.relation == "calls").count(), 0);
        assert_eq!(ex.edges.iter().filter(|e| e.relation == "uses").count(), 0);
    }

    #[test]
    fn class_and_interface_share_same_stem_prefix() {
        // Both should have nodes with stem prefix from the filename.
        let src = "<?php\nclass Vehicle {}\ninterface Drivable {}";
        let ex = extract(src, "vehicles.php");
        assert!(has_node(&ex, "vehicles_vehicle"));
        assert!(has_node(&ex, "vehicles_drivable"));
        // Total nodes: file + class + interface = 3
        assert_eq!(ex.nodes.len(), 3);
    }

    #[test]
    fn function_node_is_not_emitted_without_name_field() {
        // Closure / anonymous functions have no `name` field — they must be skipped.
        let src = "<?php\n$fn = function() {};";
        let ex = extract(src, "anon.php");
        // Only the file node; no function node for anonymous function.
        assert_eq!(
            ex.nodes.len(),
            1,
            "anonymous function must not emit a function node"
        );
    }

    #[test]
    fn class_node_count_matches_declared_classes() {
        let src = "<?php\nclass X {}\nclass Y {}\nclass Z {}";
        let ex = extract(src, "three.php");
        let class_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "three").collect();
        assert_eq!(class_nodes.len(), 3, "expected exactly 3 class nodes");
    }

    #[test]
    fn final_class_is_extracted_same_as_regular_class() {
        let src = "<?php\nfinal class Config { public function get() {} }";
        let ex = extract(src, "cfg.php");
        assert!(has_node(&ex, "cfg_config"), "final class must be extracted");
        assert!(
            has_node(&ex, "cfg_config_get"),
            "final class method must be extracted"
        );
    }

    #[test]
    fn interface_method_signatures_are_extracted() {
        let src = "<?php\ninterface Countable { public function count(): int; }";
        let ex = extract(src, "count.php");
        // Interface method signatures (even without body) should be extracted as method nodes.
        assert!(
            has_node(&ex, "count_countable_count"),
            "interface method signature must produce a method node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn deeply_underscore_method_name_fallback() {
        // A method named `___` (only underscores) should fall back to "___" after trim_matches.
        // The format string `{b}_{c}_{m}` joins with literal `_` separators, so the resulting
        // label is "mod" + "_" + "foo" + "_" + "___" = "mod_foo____" (4 trailing underscores).
        let src = "<?php\nclass Foo { public function ___() {} }";
        let ex = extract(src, "mod.php");
        // method_id("___") → trim_matches('_') → "" → fallback to "___"
        assert!(
            has_node(&ex, "mod_foo____"),
            "all-underscore method name must fall back to lowercased raw; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn file_node_source_file_is_exact_path_string() {
        let ex = extract("<?php\n", "some/path/to/file.php");
        assert_eq!(
            ex.nodes[0].source_file, "some/path/to/file.php",
            "file node source_file must be the exact path string passed in"
        );
    }

    #[test]
    fn trait_produces_no_inherits_edges() {
        // PHP traits have no extends/implements; trait `use` inside class is NOT inheritance.
        let src = "<?php\ntrait HasTimestamps { public function touch() {} }";
        let ex = extract(src, "ts.php");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "traits must not produce any inherits edges"
        );
    }
}
