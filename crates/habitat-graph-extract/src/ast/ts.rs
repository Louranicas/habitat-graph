//! TypeScript AST extractor (`tree-sitter-typescript`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; function nodes
//! `B_<fn>`; class, abstract-class, interface, and enum nodes `B_<name>`; method nodes
//! `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class→method), `inherits`
//! (base→derived, with local/external split for both `extends_clause` and `implements_clause`
//! identifiers), `imports_from` (file→module specifier). `calls` and `uses` are deliberately
//! **NOT** emitted (they are name-resolution heuristics that would not match graphify).
//!
//! Both `ts`/`mts`/`cts` (TypeScript) and `tsx` (TypeScript+JSX) dialects are handled via
//! `tree-sitter-typescript`'s two language objects; `language_for` selects the right one.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from TypeScript / TSX source using `tree-sitter-typescript`, in
/// graphify's qualified-id taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (filename without extension, lowercased): one file node `B`;
/// class / abstract-class / interface / enum nodes `B_<name>`; function nodes `B_<fn>`; method
/// nodes `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class→method), `inherits`
/// (base→derived, with `extends` and `implements` both contributing), `imports_from`
/// (file→module string). `calls`/`uses` are deliberately omitted.
#[derive(Debug, Default, Clone, Copy)]
pub struct TsExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Selects the TypeScript or TSX tree-sitter grammar based on the file extension.
///
/// Files with extension `tsx` (case-insensitive) use the JSX-aware TSX grammar; all other
/// TypeScript variants (`ts`, `mts`, `cts`) use the standard TypeScript grammar.
fn language_for(path: &Path) -> tree_sitter::Language {
    let is_tsx = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("tsx"));
    if is_tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

/// Returns the method identifier component `m` of the qualified id `B_C_m`.
///
/// Strips all leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting entirely of
/// underscores).
///
/// Examples: `_private` → `"private"`, `__init__` → `"init"`, `myMethod` → `"mymethod"`,
/// `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Collects the lowercased names of all locally-declared top-level classes, abstract classes,
/// and interfaces (including exported ones).
///
/// This first pass is used to distinguish local bases (same file) from external ones when emitting
/// `inherits` edges: a local base `Foo` in file with stem `b` gets `base_id = "b_foo"`, while an
/// external base gets `base_id = "foo"`.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        // Unwrap an export_statement to its declaration, if applicable.
        let decl = if matches!(
            child.kind(),
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
        ) {
            child
        } else if child.kind() == "export_statement" {
            let Some(d) = child.child_by_field_name("declaration") else {
                continue;
            };
            if matches!(
                d.kind(),
                "class_declaration" | "abstract_class_declaration" | "interface_declaration"
            ) {
                d
            } else {
                continue;
            }
        } else {
            continue;
        };
        if let Some(name_node) = decl.child_by_field_name("name") {
            names.insert(text_of(source, &name_node).to_lowercase());
        }
    }
    names
}

/// Dispatches a declaration node to the appropriate extraction function.
///
/// Handles `function_declaration`, `class_declaration`, `abstract_class_declaration`,
/// `interface_declaration`, `enum_declaration`, `lexical_declaration`, and `variable_declaration`.
/// All other node kinds are silently ignored.
fn dispatch_decl(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    match node.kind() {
        "function_declaration" => {
            extract_function(node, source, b, source_file, result);
        }
        "class_declaration" | "abstract_class_declaration" => {
            extract_class(node, source, b, source_file, local_types, result);
        }
        "interface_declaration" => {
            extract_interface(node, source, b, source_file, result);
        }
        "enum_declaration" => {
            extract_enum(node, source, b, source_file, result);
        }
        "lexical_declaration" | "variable_declaration" => {
            extract_var_fns(node, source, b, source_file, result);
        }
        _ => {}
    }
}

/// Extracts one `function_declaration` into `result`.
///
/// Emits a function node `B_fn` (lowercased) and a `contains` edge from `B` to the node.
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

/// Extracts one `class_declaration` or `abstract_class_declaration` into `result`.
///
/// Emits:
/// - A class node `B_c` (lowercased class name).
/// - A `contains` edge from `B` to the class node.
/// - One `inherits` edge per plain-identifier base in `extends_clause` (generic/member-expression
///   bases are silently skipped).
/// - One `inherits` edge per `type_identifier` in `implements_clause` (generic implementations
///   are silently skipped).
/// - One method node `B_c_m` and one `method` edge per `method_definition` in the class body.
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

    // Walk named children to find the class_heritage node (it is not in a field, just a child).
    for idx in 0..node.named_child_count() {
        let Some(child) = node.named_child(idx) else {
            continue;
        };
        if child.kind() == "class_heritage" {
            extract_heritage(&child, source, b, &class_label, local_types, result);
        }
    }

    // Walk the class body for method_definition children.
    if let Some(body) = node.child_by_field_name("body") {
        extract_class_methods(&body, source, b, &c, &class_label, source_file, result);
    }
}

/// Extracts `inherits` edges from a `class_heritage` node.
///
/// Emits `inherits(base_id → class_label)` for:
/// - Plain `identifier` values in each `extends_clause` (member-expression and generic bases are
///   skipped).
/// - `type_identifier` children of each `implements_clause` (generic type implementations are
///   skipped).
///
/// The `base_id` follows the local/external split: a base whose lowercased name is in
/// `local_types` (declared in the same file) gets `"b_<base>"`, an external base gets `"<base>"`.
fn extract_heritage(
    heritage: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    class_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for ci in 0..heritage.named_child_count() {
        let Some(clause) = heritage.named_child(ci) else {
            continue;
        };
        match clause.kind() {
            "extends_clause" => {
                // The value field of extends_clause is the base class expression.
                // We only handle plain identifier nodes; member_expression and generic_type are
                // skipped (analogous to Python's attribute/subscript skip).
                for vi in 0..clause.named_child_count() {
                    let Some(base_node) = clause.named_child(vi) else {
                        continue;
                    };
                    if base_node.kind() == "identifier" {
                        let base_lower = text_of(source, &base_node).to_lowercase();
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
            "implements_clause" => {
                // Named children of implements_clause are type nodes; we only handle plain
                // type_identifier children (generic_type etc. are skipped).
                for vi in 0..clause.named_child_count() {
                    let Some(iface_node) = clause.named_child(vi) else {
                        continue;
                    };
                    if iface_node.kind() == "type_identifier" {
                        let base_lower = text_of(source, &iface_node).to_lowercase();
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
            _ => {}
        }
    }
}

/// Extracts `method_definition` children of a `class_body` into `result`.
///
/// Emits a method node `B_c_m` and a `method` edge from `class_label` to the method node for
/// each direct `method_definition` child. `abstract_method_signature`, `method_signature`, and
/// computed property names are silently skipped.
fn extract_class_methods(
    body: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    c: &str,
    class_label: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for idx in 0..body.named_child_count() {
        let Some(method_node) = body.named_child(idx) else {
            continue;
        };
        if method_node.kind() != "method_definition" {
            continue;
        }
        let Some(mname_node) = method_node.child_by_field_name("name") else {
            continue;
        };
        // Computed property names (e.g. `[Symbol.iterator]`) have no plain text identity.
        if mname_node.kind() == "computed_property_name" {
            continue;
        }
        let raw_method = text_of(source, &mname_node);
        let m = method_id(&raw_method);
        let method_label = format!("{b}_{c}_{m}");
        result.nodes.push(RawNode {
            label: method_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&method_node),
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
/// Emits an interface node `B_<iface>` and a `contains` edge from `B`. Interface members are not
/// individually emitted (the taxonomy labels interfaces as monolithic symbols).
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
/// Emits an enum node `B_<enum>` and a `contains` edge from `B`.
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

/// Extracts `variable_declarator` children of a `lexical_declaration` or `variable_declaration`
/// whose `value` is an `arrow_function` or `function_expression`.
///
/// Emits a function node `B_<name>` and a `contains` edge from `B` for each qualifying
/// declarator. Declarators without a plain `identifier` name (e.g. destructuring patterns) or
/// whose value is not a function are silently skipped.
fn extract_var_fns(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for idx in 0..node.named_child_count() {
        let Some(decl) = node.named_child(idx) else {
            continue;
        };
        if decl.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = decl.child_by_field_name("name") else {
            continue;
        };
        // Only plain identifier names; skip destructuring patterns (array_pattern, object_pattern).
        if name_node.kind() != "identifier" {
            continue;
        }
        let Some(value_node) = decl.child_by_field_name("value") else {
            continue;
        };
        if !matches!(value_node.kind(), "arrow_function" | "function_expression") {
            continue;
        }
        let fn_lower = text_of(source, &name_node).to_lowercase();
        let fn_label = format!("{b}_{fn_lower}");
        result.nodes.push(RawNode {
            label: fn_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&decl),
        });
        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: fn_label,
            relation: "contains".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

/// Extracts one `import_statement` into `result`.
///
/// Emits an `imports_from` edge from `B` to the lowercased, quote-stripped module specifier
/// string. Side-effect-only imports (`import 'module'`) are included; the source field is always
/// present for well-formed import statements.
fn extract_import(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    result: &mut Extraction,
) {
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let raw = text_of(source, &source_node);
    // Strip surrounding single or double quotes; the resulting module path is then lowercased.
    let module = raw
        .trim_matches(|c: char| c == '\'' || c == '"')
        .to_lowercase();
    if module.is_empty() {
        return;
    }
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: module,
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for TsExtractor {
    fn language(&self) -> &'static str {
        "typescript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ts", "tsx", "mts", "cts"]
    }

    /// Extracts TypeScript nodes/edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased); class,
    /// abstract-class, interface, and enum nodes `B_<name>`; function nodes `B_<fn>`; method nodes
    /// `B_<cls>_<method>`; with `contains`, `method`, `inherits`, and `imports_from` edges.
    /// `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The TypeScript/TSX grammar could not be installed on the parser (should never occur with
    ///   a correctly linked `tree-sitter-typescript`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid syntax; tree-sitter
    ///   is error-tolerant and always produces a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(path))
            .map_err(|e| GraphError::Parse {
                file: source_file.clone(),
                message: e.to_string(),
            })?;
        let tree = parser.parse(source, None).ok_or_else(|| GraphError::Parse {
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
                "import_statement" => {
                    extract_import(&child, source, &b, &mut result);
                }
                "export_statement" => {
                    // Descend to the declaration inside the export, if any.  A bare re-export
                    // (`export { foo } from 'bar'`) has no declaration field and is silently
                    // ignored (no crash).
                    if let Some(decl) = child.child_by_field_name("declaration") {
                        dispatch_decl(&decl, source, &b, &source_file, &local_types, &mut result);
                    }
                }
                _ => {
                    dispatch_decl(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_types,
                        &mut result,
                    );
                }
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

    use super::TsExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        TsExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("ts extractor failed on {filename}: {e}"))
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
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "client.ts");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "client");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn tsx_source_parses_without_error_and_yields_file_node() {
        let ex = extract("const x = <div/>;\n", "view.tsx");
        assert_eq!(ex.nodes[0].label, "view");
    }

    #[test]
    fn malformed_source_returns_ok_tree_sitter_is_error_tolerant() {
        // tree-sitter always returns a (partial) tree even for invalid input.
        let ex = extract("!!!! NOT VALID TYPESCRIPT @@@", "broken.ts");
        assert!(has_node(&ex, "broken"), "file node must be present for garbled source");
    }

    // ── B. File stem handling ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HttpClient.ts");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_with_all_caps_fully_lowercased() {
        let ex = extract("", "APIUTILS.ts");
        assert_eq!(ex.nodes[0].label, "apiutils");
    }

    #[test]
    fn file_stem_preserves_underscores_and_digits() {
        let ex = extract("", "my_module2.ts");
        assert_eq!(ex.nodes[0].label, "my_module2");
    }

    #[test]
    fn mts_extension_handled() {
        let exts = TsExtractor.extensions();
        assert!(exts.contains(&"mts"), "mts extension must be registered");
    }

    #[test]
    fn cts_extension_handled() {
        let exts = TsExtractor.extensions();
        assert!(exts.contains(&"cts"), "cts extension must be registered");
    }

    #[test]
    fn language_slug_is_typescript() {
        assert_eq!(TsExtractor.language(), "typescript");
    }

    // ── C. Function declarations ───────────────────────────────────────────────────────────────

    #[test]
    fn function_declaration_emits_fn_node_and_contains_edge() {
        let ex = extract("function hello(): void {}", "utils.ts");
        assert!(has_node(&ex, "utils_hello"), "function node missing");
        assert!(
            has_edge(&ex, "utils", "utils_hello", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased_in_label() {
        let ex = extract("function ParseXML(): void {}", "p.ts");
        assert!(
            has_node(&ex, "p_parsexml"),
            "function label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_top_level_functions_all_emitted() {
        let ex = extract("function a() {} function b() {} function c() {}", "f.ts");
        assert!(has_node(&ex, "f_a"));
        assert!(has_node(&ex, "f_b"));
        assert!(has_node(&ex, "f_c"));
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "f").collect();
        assert_eq!(fn_nodes.len(), 3, "expected 3 function nodes");
    }

    #[test]
    fn function_on_third_line_has_correct_start_line() {
        let src = "\n\nfunction late(): void {}";
        let ex = extract(src, "sl.ts");
        let n = node(&ex, "sl_late");
        assert_eq!(n.span.start_line, 3, "function on line 3 must have start_line=3");
    }

    #[test]
    fn function_span_is_well_formed_and_non_empty() {
        let ex = extract("function foo() { return 1; }", "x.ts");
        let n = node(&ex, "x_foo");
        assert!(n.span.is_well_formed(), "span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "span must not be empty: {n:?}");
    }

    // ── D. Class declarations ──────────────────────────────────────────────────────────────────

    #[test]
    fn class_declaration_emits_class_node_and_contains_edge() {
        let ex = extract("class HTTPError {}", "errors.ts");
        assert!(has_node(&ex, "errors_httperror"), "class node missing");
        assert!(
            has_edge(&ex, "errors", "errors_httperror", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn class_name_is_fully_lowercased_in_label() {
        let ex = extract("class XMLParser {}", "parser.ts");
        assert!(
            has_node(&ex, "parser_xmlparser"),
            "class label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_top_level_classes_all_emitted() {
        let ex = extract("class A {} class B {} class C {}", "classes.ts");
        assert!(has_node(&ex, "classes_a"));
        assert!(has_node(&ex, "classes_b"));
        assert!(has_node(&ex, "classes_c"));
    }

    #[test]
    fn class_with_no_heritage_produces_no_inherits_edges() {
        let ex = extract("class Standalone {}", "s.ts");
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "class with no heritage must not produce inherits edges"
        );
    }

    #[test]
    fn abstract_class_emits_class_node_and_contains_edge() {
        let ex = extract("abstract class Shape { move() {} }", "shapes.ts");
        assert!(has_node(&ex, "shapes_shape"), "abstract class node missing");
        assert!(
            has_edge(&ex, "shapes", "shapes_shape", "contains"),
            "contains edge missing for abstract class"
        );
    }

    // ── E. Method extraction ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_emits_method_node_and_method_edge() {
        let ex = extract("class Foo { bark() {} }", "dog.ts");
        assert!(has_node(&ex, "dog_foo_bark"), "method node missing");
        assert!(
            has_edge(&ex, "dog_foo", "dog_foo_bark", "method"),
            "method edge missing"
        );
    }

    #[test]
    fn method_name_is_lowercased_in_label() {
        let ex = extract("class Foo { myMethod() {} }", "m.ts");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_methods_in_class_all_emitted() {
        let ex = extract(
            "class Foo { alpha() {} beta() {} gamma() {} }",
            "methods.ts",
        );
        assert!(has_node(&ex, "methods_foo_alpha"));
        assert!(has_node(&ex, "methods_foo_beta"));
        assert!(has_node(&ex, "methods_foo_gamma"));
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn method_label_format_is_b_underscore_cls_underscore_method() {
        let ex = extract("class MyClass { doWork() {} }", "mymod.ts");
        assert!(
            has_node(&ex, "mymod_myclass_dowork"),
            "method label must match b_cls_method format"
        );
    }

    #[test]
    fn underscore_prefix_stripped_from_method_id() {
        let ex = extract("class Foo { _private() {} }", "mod.ts");
        assert!(
            has_node(&ex, "mod_foo_private"),
            "leading underscore must be stripped from method id; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let ex = extract("class Dog { run() {} }", "pet.ts");
        // The method edge must go class → method, not file → method.
        assert!(has_edge(&ex, "pet_dog", "pet_dog_run", "method"));
        assert!(
            !has_edge(&ex, "pet", "pet_dog_run", "method"),
            "method edge source must be the class label, not the file label"
        );
    }

    #[test]
    fn constructor_is_extracted_as_method() {
        let ex = extract("class Widget { constructor() {} }", "w.ts");
        assert!(
            has_node(&ex, "w_widget_constructor"),
            "constructor must be extracted as a method"
        );
        assert!(has_edge(&ex, "w_widget", "w_widget_constructor", "method"));
    }

    // ── F. Heritage / inherits ─────────────────────────────────────────────────────────────────

    #[test]
    fn extends_external_class_emits_inherits_edge_with_bare_base_id() {
        let ex = extract("class Dog extends Animal {}", "m.ts");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external extends must yield bare lowercased base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_local_class_emits_inherits_edge_with_stem_prefixed_base_id() {
        let ex = extract("class A {} class B extends A {}", "m.ts");
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local extends must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn implements_external_interface_emits_inherits_edge() {
        let ex = extract("class Dog implements Runnable {}", "m.ts");
        assert!(
            has_edge(&ex, "runnable", "m_dog", "inherits"),
            "external implements must emit inherits edge"
        );
    }

    #[test]
    fn implements_local_interface_emits_inherits_edge_with_stem_prefix() {
        let src = "interface Runnable { run(): void; } class Dog implements Runnable {}";
        let ex = extract(src, "m.ts");
        assert!(
            has_edge(&ex, "m_runnable", "m_dog", "inherits"),
            "local implements must use stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn both_extends_and_implements_emit_both_inherits_edges() {
        let ex = extract(
            "class Dog extends Animal implements Runnable {}",
            "m.ts",
        );
        assert!(has_edge(&ex, "animal", "m_dog", "inherits"), "extends edge missing");
        assert!(has_edge(&ex, "runnable", "m_dog", "inherits"), "implements edge missing");
        let inherits: Vec<_> = ex.edges.iter().filter(|e| e.relation == "inherits").collect();
        assert_eq!(inherits.len(), 2, "expected exactly 2 inherits edges");
    }

    #[test]
    fn multiple_implements_all_emit_inherits_edges() {
        let ex = extract(
            "class Foo implements Bar, Baz, Qux {}",
            "m.ts",
        );
        assert!(has_edge(&ex, "bar", "m_foo", "inherits"));
        assert!(has_edge(&ex, "baz", "m_foo", "inherits"));
        assert!(has_edge(&ex, "qux", "m_foo", "inherits"));
        let inherits = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(inherits, 3, "expected 3 inherits edges from multiple implements");
    }

    #[test]
    fn generic_extends_still_emits_inherits_for_base_identifier() {
        // In the TypeScript grammar, `extends Array<T>` is parsed as `value: (identifier)` +
        // `type_arguments: (type_arguments ...)` separately. The base class identifier "Array"
        // IS captured as a plain identifier, so an inherits edge is correctly emitted.
        // (This differs from Python where `Generic[T]` parses as a subscript and is skipped.)
        let ex = extract("class Stack<T> extends Array<T> {}", "s.ts");
        assert!(
            has_edge(&ex, "array", "s_stack", "inherits"),
            "generic extends must still emit inherits edge for the base identifier; got {:?}",
            ex.edges
        );
    }

    #[test]
    fn local_and_external_bases_mixed_are_classified_correctly() {
        let src = "class Base {} class Child extends Base implements External {}";
        let ex = extract(src, "mix.ts");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
        assert!(
            has_edge(&ex, "external", "mix_child", "inherits"),
            "external base must use bare lowercased id"
        );
    }

    // ── G. Interface declarations ──────────────────────────────────────────────────────────────

    #[test]
    fn interface_declaration_emits_iface_node_and_contains_edge() {
        let ex = extract("interface User { name: string; }", "models.ts");
        assert!(has_node(&ex, "models_user"), "interface node missing");
        assert!(
            has_edge(&ex, "models", "models_user", "contains"),
            "contains edge missing for interface"
        );
    }

    #[test]
    fn interface_name_is_lowercased_in_label() {
        let ex = extract("interface HTTPClient {}", "iface.ts");
        assert!(has_node(&ex, "iface_httpclient"), "interface label must be lowercased");
    }

    #[test]
    fn multiple_interfaces_all_emitted() {
        let ex = extract(
            "interface A {} interface B {} interface C {}",
            "ifaces.ts",
        );
        assert!(has_node(&ex, "ifaces_a"));
        assert!(has_node(&ex, "ifaces_b"));
        assert!(has_node(&ex, "ifaces_c"));
        let contains = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(contains, 3, "expected 3 contains edges");
    }

    // ── H. Enum declarations ───────────────────────────────────────────────────────────────────

    #[test]
    fn enum_declaration_emits_enum_node_and_contains_edge() {
        let ex = extract("enum Color { Red, Green, Blue }", "enums.ts");
        assert!(has_node(&ex, "enums_color"), "enum node missing");
        assert!(
            has_edge(&ex, "enums", "enums_color", "contains"),
            "contains edge missing for enum"
        );
    }

    #[test]
    fn enum_name_is_lowercased_in_label() {
        let ex = extract("enum HTTPMethod { GET, POST }", "http.ts");
        assert!(has_node(&ex, "http_httpmethod"), "enum label must be lowercased");
    }

    // ── I. Variable / arrow functions ──────────────────────────────────────────────────────────

    #[test]
    fn const_arrow_function_emits_fn_node_and_contains_edge() {
        let ex = extract("const greet = (name: string) => { return name; };", "utils.ts");
        assert!(has_node(&ex, "utils_greet"), "arrow fn node missing");
        assert!(
            has_edge(&ex, "utils", "utils_greet", "contains"),
            "contains edge for arrow fn missing"
        );
    }

    #[test]
    fn const_function_expression_emits_fn_node() {
        let ex = extract("const add = function(a: number, b: number) { return a + b; };", "math.ts");
        assert!(has_node(&ex, "math_add"), "fn expr node missing");
        assert!(has_edge(&ex, "math", "math_add", "contains"));
    }

    #[test]
    fn var_arrow_function_emits_fn_node() {
        let ex = extract("var compute = () => 42;", "v.ts");
        assert!(has_node(&ex, "v_compute"), "var arrow fn node missing");
    }

    #[test]
    fn const_with_non_fn_value_does_not_emit_fn_node() {
        let ex = extract("const MAX = 42; const NAME = 'Alice';", "consts.ts");
        // No function values → no additional nodes beyond the file node.
        assert_eq!(
            ex.nodes.len(),
            1,
            "const with non-fn value must not emit a node; got {:?}",
            ex.nodes
        );
    }

    #[test]
    fn arrow_fn_name_is_lowercased() {
        let ex = extract("const MyFunc = () => {};", "n.ts");
        assert!(has_node(&ex, "n_myfunc"), "arrow fn label must be lowercased");
    }

    // ── J. Export statements ───────────────────────────────────────────────────────────────────

    #[test]
    fn export_function_declaration_is_extracted() {
        let ex = extract("export function greet(): void {}", "api.ts");
        assert!(has_node(&ex, "api_greet"), "exported fn node missing");
        assert!(has_edge(&ex, "api", "api_greet", "contains"));
    }

    #[test]
    fn export_class_declaration_is_extracted() {
        let ex = extract("export class Client {}", "client.ts");
        assert!(has_node(&ex, "client_client"), "exported class node missing");
        assert!(has_edge(&ex, "client", "client_client", "contains"));
    }

    #[test]
    fn export_interface_is_extracted() {
        let ex = extract("export interface Repo {}", "repo.ts");
        assert!(has_node(&ex, "repo_repo"), "exported interface node missing");
    }

    #[test]
    fn export_enum_is_extracted() {
        let ex = extract("export enum Status { Ok, Fail }", "status.ts");
        assert!(has_node(&ex, "status_status"), "exported enum node missing");
    }

    #[test]
    fn bare_re_export_does_not_crash_and_file_node_present() {
        // `export { foo } from 'bar'` has no declaration field — must not panic.
        let ex = extract("export { foo } from 'bar';", "reexport.ts");
        assert!(has_node(&ex, "reexport"), "file node must be present");
    }

    // ── K. Import statements ───────────────────────────────────────────────────────────────────

    #[test]
    fn import_named_emits_imports_from_edge() {
        let ex = extract("import { Client } from 'httpx';", "c.ts");
        assert!(
            has_edge(&ex, "c", "httpx", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_default_emits_imports_from_edge() {
        let ex = extract("import React from 'react';", "app.ts");
        assert!(has_edge(&ex, "app", "react", "imports_from"));
    }

    #[test]
    fn import_star_namespace_emits_imports_from_edge() {
        let ex = extract("import * as ns from 'lib';", "n.ts");
        assert!(has_edge(&ex, "n", "lib", "imports_from"));
    }

    #[test]
    fn import_source_is_lowercased() {
        let ex = extract("import { X } from 'HTTP.Client';", "c.ts");
        assert!(
            has_edge(&ex, "c", "http.client", "imports_from"),
            "import source must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_imports_all_emit_imports_from_edges() {
        let ex = extract(
            "import { a } from 'alpha';\nimport { b } from 'beta';\nimport { c } from 'gamma';",
            "multi.ts",
        );
        assert!(has_edge(&ex, "multi", "alpha", "imports_from"));
        assert!(has_edge(&ex, "multi", "beta", "imports_from"));
        assert!(has_edge(&ex, "multi", "gamma", "imports_from"));
        let imports = ex.edges.iter().filter(|e| e.relation == "imports_from").count();
        assert_eq!(imports, 3, "expected 3 imports_from edges");
    }

    // ── L. Negative invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = concat!(
            "function foo() { bar(); }\n",
            "function bar() {}\n",
            "class A { m() { this.n(); } n() {} }\n",
        );
        let ex = extract(src, "neg.ts");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "const x = 1; function useX() { return x; }";
        let ex = extract(src, "neg2.ts");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn methods_get_method_edge_not_contains_edge() {
        let ex = extract("class Foo { go() {} }", "f.ts");
        // The method node must appear only via a 'method' edge, not a 'contains' edge.
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

    // ── M. Edge properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "import { x } from 'mod';\n",
            "class Foo extends Bar implements Baz { m() {} }\n",
            "function fn1() {}\n",
        );
        let ex = extract(src, "conf.ts");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Extracted confidence; got {edge:?}"
            );
        }
    }

    // ── N. Span properties ─────────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let ex = extract("class Foo {}", "span.ts");
        let n = node(&ex, "span");
        assert_eq!(n.span.start_byte, 0, "file node span must start at byte 0");
    }

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let ex = extract("class Foo {}", "x.ts");
        let n = node(&ex, "x_foo");
        assert_eq!(n.span.start_line, 1, "class on line 1 must have start_line=1");
    }

    #[test]
    fn class_on_third_line_has_correct_start_line() {
        let src = "\n\nclass Late {}";
        let ex = extract(src, "y.ts");
        let n = node(&ex, "y_late");
        assert_eq!(n.span.start_line, 3, "class on line 3 must have start_line=3");
    }

    #[test]
    fn all_node_spans_are_well_formed_and_non_empty_for_non_trivial_source() {
        let src = "class A { run() {} } function b() { return 1; }";
        let ex = extract(src, "ws.ts");
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
        let src = "class C { m() {} }\nfunction f() {}";
        let ex = extract(src, "traced.ts");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("traced.ts"),
                "source_file {:?} must contain 'traced.ts'",
                n.source_file
            );
        }
    }

    // ── O. Mixed / composite ───────────────────────────────────────────────────────────────────

    #[test]
    fn mixed_class_function_import_all_emitted() {
        let src = concat!(
            "import { path } from 'node:path';\n",
            "class Parser {}\n",
            "function parse(): void {}\n",
        );
        let ex = extract(src, "mix.ts");
        assert!(has_node(&ex, "mix"), "file node missing");
        assert!(has_node(&ex, "mix_parser"), "class node missing");
        assert!(has_node(&ex, "mix_parse"), "function node missing");
        assert!(has_edge(&ex, "mix", "mix_parser", "contains"));
        assert!(has_edge(&ex, "mix", "mix_parse", "contains"));
        assert!(has_edge(&ex, "mix", "node:path", "imports_from"));
    }

    #[test]
    fn exported_class_with_method_and_local_base_fully_connected() {
        let src = concat!(
            "class Base { init() {} }\n",
            "export class Derived extends Base { work() {} }\n",
        );
        let ex = extract(src, "chain.ts");
        assert!(has_node(&ex, "chain_base"), "base class missing");
        assert!(has_node(&ex, "chain_derived"), "derived class missing");
        assert!(has_node(&ex, "chain_base_init"), "base method missing");
        assert!(has_node(&ex, "chain_derived_work"), "derived method missing");
        assert!(
            has_edge(&ex, "chain_base", "chain_derived", "inherits"),
            "inherits edge missing"
        );
    }

    #[test]
    fn two_classes_two_methods_each_produce_correct_totals() {
        let src = concat!(
            "class A { x() {} y() {} }\n",
            "class B { p() {} q() {} }\n",
        );
        let ex = extract(src, "ab.ts");
        // file + 2 classes + 4 methods = 7 nodes
        assert_eq!(ex.nodes.len(), 7, "expected 7 nodes");
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges");
    }

    #[test]
    fn abstract_class_methods_emitted_same_as_concrete() {
        let ex = extract("abstract class Shape { move() {} }", "shapes.ts");
        assert!(has_node(&ex, "shapes_shape_move"), "abstract class method node missing");
        assert!(has_edge(&ex, "shapes_shape", "shapes_shape_move", "method"));
    }

    #[test]
    fn complex_file_with_all_kinds_produces_correct_node_set() {
        let src = concat!(
            "import { fs } from 'node:fs';\n",
            "interface Logger { log(msg: string): void; }\n",
            "enum Level { Info, Warn, Error }\n",
            "class Base { baseMethod() {} }\n",
            "class App extends Base implements Logger {\n",
            "  constructor() {}\n",
            "  log(msg: string) {}\n",
            "}\n",
            "function run(): void {}\n",
            "const helper = () => {};\n",
        );
        let ex = extract(src, "app.ts");
        // file, logger, level, base, app, base_basemethod, app_constructor, app_log, run, helper
        // = 10 nodes
        assert_eq!(ex.nodes.len(), 10, "expected 10 nodes; got {:?}", ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>());
        assert!(has_node(&ex, "app"), "file node missing");
        assert!(has_node(&ex, "app_logger"), "interface node missing");
        assert!(has_node(&ex, "app_level"), "enum node missing");
        assert!(has_node(&ex, "app_base"), "base class missing");
        assert!(has_node(&ex, "app_app"), "app class missing");
        assert!(has_node(&ex, "app_base_basemethod"), "base method missing");
        assert!(has_node(&ex, "app_app_constructor"), "constructor missing");
        assert!(has_node(&ex, "app_app_log"), "log method missing");
        assert!(has_node(&ex, "app_run"), "run fn missing");
        assert!(has_node(&ex, "app_helper"), "helper arrow fn missing");
        // imports_from: node:fs
        assert!(has_edge(&ex, "app", "node:fs", "imports_from"));
        // inherits: base (local) → app, logger (local) → app
        assert!(has_edge(&ex, "app_base", "app_app", "inherits"), "extends inherits missing");
        assert!(has_edge(&ex, "app_logger", "app_app", "inherits"), "implements inherits missing");
    }

    #[test]
    fn tsx_file_extracts_class_declarations() {
        let src = "class MyComponent { render() { return null; } }";
        let ex = extract(src, "comp.tsx");
        assert!(has_node(&ex, "comp_mycomponent"), "class node missing in TSX");
        assert!(has_node(&ex, "comp_mycomponent_render"), "method node missing in TSX");
    }
}
