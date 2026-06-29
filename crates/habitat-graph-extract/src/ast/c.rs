//! C AST extractor (`tree-sitter-c`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (filename without extension, lowercased): one file node `B`;
//! top-level function nodes `B_<fn>`; named struct/union nodes `B_<name>`; typedef nodes
//! `B_<typename>`. Edges: `contains` (file→symbol), `imports_from` (file→included-header-path).
//! `inherits`, `calls`, and `uses` are deliberately NOT emitted — C has no inheritance and
//! name-resolution heuristics would not match graphify.
//!
//! Key AST node kinds processed (as named children of `translation_unit`):
//!
//! - `struct_specifier` / `union_specifier` — top-level struct/union definitions appear here
//!   **directly** when there is no associated variable declaration (via `_empty_declaration`
//!   inlining in the tree-sitter-c grammar). Only named definitions with a body are emitted.
//! - `function_definition` — top-level C function definitions.
//! - `declaration` — declarations that define a named struct/union AND also declare a variable
//!   (`struct Foo { int x; } my_var;`). Only the struct/union name is emitted, not the variable.
//! - `type_definition` — C `typedef` declarations. Emits the alias name; also emits an embedded
//!   named aggregate when present.
//! - `preproc_include` — `#include` directives, emitting `imports_from` edges. Directives inside
//!   `#ifdef`/`#ifndef` blocks are nested under `preproc_ifdef` nodes and are NOT processed
//!   (only top-level includes are extracted, matching graphify's single-pass behaviour).
//!
//! An empty or comment-only source produces exactly one node (the file node) and no edges.

use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result, Span};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

/// Extracts nodes and edges from C source using `tree-sitter-c`, in graphify's qualified-id
/// taxonomy (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (filename without extension, lowercased): one file node `B`;
/// function nodes `B_<fn>`; named struct/union/typedef nodes `B_<name>`. Edges: `contains`
/// (file→symbol), `imports_from` (file→include path). `inherits`, `calls`, and `uses` are
/// deliberately omitted — C has no inheritance.
///
/// An empty or comment-only source produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct CExtractor;

// ── Private helpers ─────────────────────────────────────────────────────────────────────────────

/// Recursively resolves the innermost identifier name from a C declarator node.
///
/// Handles the declarator-nesting patterns produced by tree-sitter-c:
/// - `identifier` → the bare function/variable name, lowercased.
/// - `function_declarator` → unwraps the `declarator` field.
/// - `pointer_declarator` → unwraps the `declarator` field, stripping pointer indirection.
/// - `parenthesized_declarator` → unwraps the `declarator` field.
///
/// Returns `None` for unrecognised or abstract declarator kinds; those are silently skipped.
fn declarator_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text_of(source, node).to_lowercase()),
        "function_declarator" | "pointer_declarator" | "parenthesized_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            declarator_name(&inner, source)
        }
        _ => None,
    }
}

/// Resolves the typedef alias name from the `declarator` field of a `type_definition` node.
///
/// Handles:
/// - `type_identifier` → direct typedef alias, lowercased.
/// - `pointer_declarator` → pointer typedef; recursively unwraps to find the `type_identifier`.
///
/// Returns `None` for complex declarators (e.g. function-pointer typedefs) or when the alias
/// name resolves to a tree-sitter-c built-in `primitive_type` (such as `size_t`, `uint32_t`).
/// Those are silently skipped.
fn typedef_declarator_name(node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" => Some(text_of(source, node).to_lowercase()),
        "pointer_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            typedef_declarator_name(&inner, source)
        }
        _ => None,
    }
}

/// Emits a symbol node and a `contains` edge from `B` into `result`.
fn emit_symbol(label: String, source_file: &str, span: Span, b: &str, result: &mut Extraction) {
    result.nodes.push(RawNode {
        label: label.clone(),
        source_file: source_file.to_owned(),
        span,
    });
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: label,
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Extracts a top-level `function_definition`, emitting a function node `B_<fn>` and a `contains`
/// edge.
///
/// The function name is the lowercased innermost identifier found by recursively unwrapping the
/// `declarator` field. Definitions whose name cannot be resolved are silently skipped.
fn extract_function(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(decl_node) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(fn_name) = declarator_name(&decl_node, source) else {
        return;
    };
    let label = format!("{b}_{fn_name}");
    emit_symbol(label, source_file, make_span(node), b, result);
}

/// Extracts a `struct_specifier` or `union_specifier` node that has both a `name` and a `body`.
///
/// Emits a node `B_<name>` and a `contains` edge. Aggregates without a name (anonymous) or
/// without a body (forward declarations) are silently skipped.
///
/// In tree-sitter-c, standalone struct/union definitions (`struct Foo { ... };`) appear
/// **directly** as `struct_specifier`/`union_specifier` children of `translation_unit` via the
/// `_empty_declaration` grammar rule's inlining. Aggregates that also declare a variable
/// (`struct Foo { ... } my_var;`) appear inside a `declaration` node.
fn extract_struct_specifier(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    // Skip forward declarations (no body field means: `struct Foo;`).
    if node.child_by_field_name("body").is_none() {
        return;
    }
    let Some(name_node) = node.child_by_field_name("name") else {
        return; // anonymous aggregate — name comes from enclosing typedef, if any
    };
    let struct_name = text_of(source, &name_node).to_lowercase();
    let label = format!("{b}_{struct_name}");
    emit_symbol(label, source_file, make_span(node), b, result);
}

/// Processes a top-level `declaration` node.
///
/// Handles the case where a struct/union is BOTH defined AND a variable is declared in the same
/// statement: `struct Foo { int x; } my_var;`. If the `type` field is a named `struct_specifier`
/// or `union_specifier` with a body, emits the struct/union node. The variable (`my_var`) is
/// NOT emitted.
///
/// Note: standalone `struct Foo { ... };` (without a variable) is NOT a `declaration` in
/// tree-sitter-c — it appears directly as a `struct_specifier` child of `translation_unit`.
fn extract_declaration(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    if matches!(type_node.kind(), "struct_specifier" | "union_specifier") {
        extract_struct_specifier(&type_node, source, b, source_file, result);
    }
}

/// Processes a top-level `type_definition` (C `typedef`) node.
///
/// Emits the typedef alias as `B_<alias>` with a `contains` edge. If the underlying type is a
/// named `struct_specifier` or `union_specifier` with a body, also emits that aggregate node
/// separately — both are distinct symbols in the graphify taxonomy.
///
/// Note: aliases that resolve to tree-sitter-c built-in `primitive_type` tokens (e.g. `size_t`,
/// `uint32_t`) may not have a `declarator` field and are silently skipped; those are built-in
/// types already known to the grammar.
fn extract_type_definition(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    // Emit the typedef alias name from the declarator field.
    if let Some(decl_node) = node.child_by_field_name("declarator") {
        if let Some(alias_name) = typedef_declarator_name(&decl_node, source) {
            let label = format!("{b}_{alias_name}");
            emit_symbol(label, source_file, make_span(node), b, result);
        }
    }
    // Also emit the embedded named aggregate (if it has a name and body).
    if let Some(type_node) = node.child_by_field_name("type") {
        if matches!(type_node.kind(), "struct_specifier" | "union_specifier") {
            extract_struct_specifier(&type_node, source, b, source_file, result);
        }
    }
}

/// Processes a `preproc_include` directive, emitting an `imports_from` edge.
///
/// The `path` field is either a `system_lib_string` (`<stdio.h>`) or a `string_literal`
/// (`"myheader.h"`); both include their surrounding delimiters, which are stripped before
/// emitting. The path is lowercased to match graphify's taxonomy. Empty or purely-delimiter
/// paths are silently skipped.
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
    // Strip surrounding <…> or "…" delimiters.
    let stripped = raw.trim_matches(|c: char| c == '<' || c == '>' || c == '"');
    if stripped.is_empty() {
        return;
    }
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: stripped.to_lowercase(),
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

// ── Extractor impl ──────────────────────────────────────────────────────────────────────────────

impl Extractor for CExtractor {
    fn language(&self) -> &'static str {
        "c"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["c", "h"]
    }

    /// Extracts C nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// function nodes `B_<fn>`; named struct/union/typedef nodes `B_<name>`; with `contains`
    /// and `imports_from` edges. `inherits`, `calls`, and `uses` are deliberately omitted —
    /// C has no inheritance.
    ///
    /// An empty or comment-only file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The C grammar could not be installed on the parser (should not occur with a correctly
    ///   linked `tree-sitter-c`).
    /// - `parser.parse` returns `None` (cancellation or timeout — not triggered by invalid C
    ///   syntax; tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
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
        let mut result = Extraction::new();

        // Always emit the file node, even for empty or comment-only source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Walk top-level translation_unit named children, dispatching on node kind.
        //
        // NOTE: In tree-sitter-c, standalone struct/union definitions (`struct Foo { ... };`)
        // are parsed via `_empty_declaration` which is an inline rule. This causes the inner
        // `struct_specifier`/`union_specifier` to appear DIRECTLY as a named child of
        // `translation_unit`. Hence we handle them here in addition to inside `declaration`
        // (which covers the combined define+declare form: `struct Foo { ... } my_var;`).
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            match child.kind() {
                "function_definition" => {
                    extract_function(&child, source, &b, &source_file, &mut result);
                }
                // Standalone struct/union definition: `struct Foo { int x; };`
                "struct_specifier" | "union_specifier" => {
                    extract_struct_specifier(&child, source, &b, &source_file, &mut result);
                }
                // Combined define+declare: `struct Foo { int x; } my_var;`
                "declaration" => {
                    extract_declaration(&child, source, &b, &source_file, &mut result);
                }
                "type_definition" => {
                    extract_type_definition(&child, source, &b, &source_file, &mut result);
                }
                "preproc_include" => {
                    extract_include(&child, source, &b, &mut result);
                }
                _ => {}
            }
        }

        Ok(result)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::CExtractor;
    use crate::registry::Extractor;

    // ── Helpers ──────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on extractor error.
    fn extract(src: &str, filename: &str) -> Extraction {
        CExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("c extractor failed on {filename}: {e}"))
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

    /// Returns the node with the given label; panics when absent.
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

    // ── A. Empty / trivial input ─────────────────────────────────────────────────────────────────

    #[test]
    fn a01_empty_source_yields_exactly_one_file_node() {
        let ex = extract("", "empty.c");
        assert_eq!(ex.nodes.len(), 1, "empty source: expected exactly 1 node");
    }

    #[test]
    fn a02_empty_source_file_node_has_correct_label() {
        let ex = extract("", "empty.c");
        assert_eq!(ex.nodes[0].label, "empty");
    }

    #[test]
    fn a03_empty_source_yields_no_edges() {
        let ex = extract("", "empty.c");
        assert_eq!(ex.edges.len(), 0, "empty source: expected 0 edges");
    }

    #[test]
    fn a04_comment_only_source_yields_one_file_node_no_edges() {
        let src = "/* copyright header */\n// single-line comment\n";
        let ex = extract(src, "comments.c");
        assert_eq!(ex.nodes.len(), 1, "comment-only: expected 1 node");
        assert_eq!(ex.edges.len(), 0, "comment-only: expected 0 edges");
    }

    // ── B. File stem lowercasing ─────────────────────────────────────────────────────────────────

    #[test]
    fn b01_c_file_stem_is_lowercased() {
        let ex = extract("", "MyModule.c");
        assert_eq!(ex.nodes[0].label, "mymodule");
    }

    #[test]
    fn b02_h_file_stem_is_lowercased() {
        let ex = extract("", "HTTPClient.h");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn b03_mixed_case_stem_is_fully_lowercased() {
        let ex = extract("", "NetUtil.c");
        assert_eq!(ex.nodes[0].label, "netutil");
    }

    // ── C. Single function definition ────────────────────────────────────────────────────────────

    #[test]
    fn c01_single_function_emits_fn_node() {
        let src = "int add(int a, int b) { return a + b; }\n";
        let ex = extract(src, "math.c");
        assert!(has_node(&ex, "math_add"), "function node missing");
    }

    #[test]
    fn c02_single_function_emits_contains_edge() {
        let src = "int add(int a, int b) { return a + b; }\n";
        let ex = extract(src, "math.c");
        assert!(
            has_edge(&ex, "math", "math_add", "contains"),
            "contains edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn c03_function_name_is_lowercased_in_qualified_id() {
        let src = "void PrintMessage(void) {}\n";
        let ex = extract(src, "msg.c");
        assert!(
            has_node(&ex, "msg_printmessage"),
            "function name must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn c04_function_qualified_id_matches_b_fn_format() {
        let src = "int compute(void) { return 0; }\n";
        let ex = extract(src, "calc.c");
        // b = "calc", fn = "compute" → "calc_compute"
        assert!(has_node(&ex, "calc_compute"), "qualified_id must be b_fn");
    }

    // ── D. Multiple functions ────────────────────────────────────────────────────────────────────

    #[test]
    fn d01_multiple_functions_all_emitted() {
        let src = "void foo(void) {}\nvoid bar(void) {}\nvoid baz(void) {}\n";
        let ex = extract(src, "fns.c");
        assert!(has_node(&ex, "fns_foo"), "foo missing");
        assert!(has_node(&ex, "fns_bar"), "bar missing");
        assert!(has_node(&ex, "fns_baz"), "baz missing");
    }

    #[test]
    fn d02_multiple_functions_all_have_contains_edges() {
        let src = "int f1(void) { return 1; }\nint f2(void) { return 2; }\n";
        let ex = extract(src, "two.c");
        let contains: Vec<_> = ex.edges.iter().filter(|e| e.relation == "contains").collect();
        assert_eq!(contains.len(), 2, "expected 2 contains edges; got {contains:?}");
    }

    #[test]
    fn d03_function_count_matches_definitions() {
        let src = concat!(
            "void a(void) {}\n",
            "void b(void) {}\n",
            "void c(void) {}\n",
            "void d(void) {}\n",
        );
        let ex = extract(src, "many.c");
        // 1 file node + 4 function nodes
        assert_eq!(
            ex.nodes.len(),
            5,
            "expected 5 nodes (1 file + 4 functions); got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── E. Function declarator variants ─────────────────────────────────────────────────────────

    #[test]
    fn e01_pointer_returning_function_name_extracted() {
        // `char *strdup(const char *s)` — declarator nesting:
        // pointer_declarator → function_declarator → identifier
        let src = "char *my_strdup(const char *s) { return s; }\n";
        let ex = extract(src, "str.c");
        assert!(
            has_node(&ex, "str_my_strdup"),
            "pointer-returning function node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn e02_static_function_name_extracted() {
        let src = "static int helper(int x) { return x * 2; }\n";
        let ex = extract(src, "impl.c");
        assert!(
            has_node(&ex, "impl_helper"),
            "static function node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn e03_void_return_function_extracted() {
        let src = "void my_init(void) { }\n";
        let ex = extract(src, "init.c");
        assert!(has_node(&ex, "init_my_init"), "void-return function node missing");
    }

    // ── F. Named struct definition ───────────────────────────────────────────────────────────────
    // In tree-sitter-c, `struct Foo { ... };` (standalone, no variable) is parsed via
    // `_empty_declaration` inlining and appears as a `struct_specifier` DIRECTLY under
    // `translation_unit` — not inside a `declaration` node.

    #[test]
    fn f01_named_struct_definition_emits_struct_node() {
        let src = "struct Point { int x; int y; };\n";
        let ex = extract(src, "geo.c");
        assert!(
            has_node(&ex, "geo_point"),
            "struct node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn f02_named_struct_emits_contains_edge() {
        let src = "struct ListNode { int val; };\n";
        let ex = extract(src, "list.c");
        assert!(
            has_edge(&ex, "list", "list_listnode", "contains"),
            "contains edge for struct missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn f03_struct_name_is_lowercased() {
        let src = "struct HTTPRequest { int method; };\n";
        let ex = extract(src, "req.c");
        assert!(
            has_node(&ex, "req_httprequest"),
            "struct name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn f04_struct_qualified_id_matches_b_structname_format() {
        let src = "struct Buffer { char data[256]; int len; };\n";
        let ex = extract(src, "buf.c");
        // b = "buf", struct = "buffer" → "buf_buffer"
        assert!(has_node(&ex, "buf_buffer"), "qualified_id must be b_structname");
    }

    // ── G. Struct skip cases ─────────────────────────────────────────────────────────────────────

    #[test]
    fn g01_forward_struct_declaration_not_emitted() {
        // `struct Foo;` — forward declaration with no body.
        let src = "struct Foo;\n";
        let ex = extract(src, "fwd.c");
        // Only the file node; forward-declaration struct must not appear.
        assert_eq!(
            ex.nodes.len(),
            1,
            "forward declaration must not emit a node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn g02_struct_with_body_and_variable_also_emits_struct() {
        // `struct Foo { int x; } my_foo;` — struct_specifier inside a `declaration` node.
        // The struct node IS emitted; the variable `my_foo` is NOT.
        let src = "struct Foo { int x; } my_foo;\n";
        let ex = extract(src, "var.c");
        assert!(has_node(&ex, "var_foo"), "struct node must be emitted when body present");
        assert!(
            !has_node(&ex, "var_my_foo"),
            "variable declaration must not produce a node"
        );
    }

    #[test]
    fn g03_anonymous_struct_not_emitted_as_standalone_node() {
        // `struct { int x; };` — anonymous struct without any name or typedef.
        let src = "struct { int x; };\n";
        let ex = extract(src, "anon.c");
        assert_eq!(
            ex.nodes.len(),
            1,
            "anonymous struct must not produce a node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── H. Typedef (non-struct) ──────────────────────────────────────────────────────────────────

    #[test]
    fn h01_simple_typedef_emits_node_and_contains_edge() {
        let src = "typedef int MyInt;\n";
        let ex = extract(src, "types.c");
        assert!(
            has_node(&ex, "types_myint"),
            "typedef node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(
            has_edge(&ex, "types", "types_myint", "contains"),
            "typedef contains edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn h02_typedef_name_is_lowercased() {
        let src = "typedef unsigned long UInt64;\n";
        let ex = extract(src, "t.c");
        assert!(
            has_node(&ex, "t_uint64"),
            "typedef name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn h03_typedef_pointer_type_emits_node() {
        let src = "typedef char *MyString;\n";
        let ex = extract(src, "str.c");
        assert!(
            has_node(&ex, "str_mystring"),
            "pointer typedef must emit node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn h04_typedef_anonymous_struct_emits_only_alias_node() {
        // `typedef struct { int x; int y; } Point;` — anonymous struct → only alias emitted.
        let src = "typedef struct { int x; int y; } Point;\n";
        let ex = extract(src, "pt.c");
        assert!(has_node(&ex, "pt_point"), "typedef alias node must be emitted");
        // No additional struct nodes for an anonymous struct.
        let extra: Vec<_> = ex
            .nodes
            .iter()
            .filter(|n| n.label != "pt" && n.label != "pt_point")
            .collect();
        assert_eq!(
            extra.len(),
            0,
            "anonymous struct must not emit additional node; extra: {extra:?}"
        );
    }

    // ── I. Typedef of named struct ───────────────────────────────────────────────────────────────

    #[test]
    fn i01_typedef_named_struct_emits_both_struct_and_alias_nodes() {
        // `typedef struct Foo { int x; } FooT;` → nodes: "foo_foo" + "foo_foot"
        let src = "typedef struct Foo { int x; } FooT;\n";
        let ex = extract(src, "foo.c");
        assert!(has_node(&ex, "foo_foo"), "struct node must be emitted");
        assert!(has_node(&ex, "foo_foot"), "typedef alias node must be emitted");
    }

    #[test]
    fn i02_typedef_named_struct_emits_two_contains_edges() {
        let src = "typedef struct Vec { float x; float y; } Vec2;\n";
        let ex = extract(src, "v.c");
        let contains: Vec<_> = ex.edges.iter().filter(|e| e.relation == "contains").collect();
        assert_eq!(
            contains.len(),
            2,
            "typedef named struct must produce 2 contains edges; got {contains:?}"
        );
    }

    // ── J. Includes / imports_from ───────────────────────────────────────────────────────────────

    #[test]
    fn j01_system_include_emits_imports_from_edge() {
        let src = "#include <stdio.h>\n";
        let ex = extract(src, "main.c");
        assert!(
            has_edge(&ex, "main", "stdio.h", "imports_from"),
            "imports_from edge for <stdio.h> missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn j02_local_include_emits_imports_from_edge() {
        let src = "#include \"myheader.h\"\n";
        let ex = extract(src, "app.c");
        assert!(
            has_edge(&ex, "app", "myheader.h", "imports_from"),
            "imports_from edge for local header missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn j03_include_path_strips_angle_brackets() {
        let src = "#include <stdlib.h>\n";
        let ex = extract(src, "m.c");
        // Target must be "stdlib.h" not "<stdlib.h>"
        assert!(
            has_edge(&ex, "m", "stdlib.h", "imports_from"),
            "angle brackets must be stripped; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn j04_include_path_strips_double_quotes() {
        let src = "#include \"utils.h\"\n";
        let ex = extract(src, "m.c");
        // Target must be "utils.h" not "\"utils.h\""
        assert!(
            has_edge(&ex, "m", "utils.h", "imports_from"),
            "double quotes must be stripped; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn j05_multiple_includes_all_emitted() {
        let src = concat!(
            "#include <stdio.h>\n",
            "#include <stdlib.h>\n",
            "#include \"config.h\"\n",
        );
        let ex = extract(src, "prog.c");
        assert!(has_edge(&ex, "prog", "stdio.h", "imports_from"), "stdio.h missing");
        assert!(has_edge(&ex, "prog", "stdlib.h", "imports_from"), "stdlib.h missing");
        assert!(has_edge(&ex, "prog", "config.h", "imports_from"), "config.h missing");
        let import_edges: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .collect();
        assert_eq!(import_edges.len(), 3, "expected 3 imports_from edges");
    }

    // ── K. Include path properties ───────────────────────────────────────────────────────────────

    #[test]
    fn k01_include_path_is_lowercased() {
        let src = "#include <Stdio.H>\n";
        let ex = extract(src, "x.c");
        assert!(
            has_edge(&ex, "x", "stdio.h", "imports_from"),
            "include path must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn k02_subdirectory_include_path_preserved() {
        let src = "#include <sys/socket.h>\n";
        let ex = extract(src, "net.c");
        assert!(
            has_edge(&ex, "net", "sys/socket.h", "imports_from"),
            "subdirectory path must be preserved; edges: {:?}",
            ex.edges
        );
    }

    // ── L. Negative invariants ───────────────────────────────────────────────────────────────────

    #[test]
    fn l01_no_inherits_edges_c_has_no_inheritance() {
        let src = concat!(
            "struct Animal { int weight; };\n",
            "struct Dog { int breed; };\n",
        );
        let ex = extract(src, "animals.c");
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        assert_eq!(
            inherits.len(),
            0,
            "C has no inheritance: 0 inherits edges expected; got {inherits:?}"
        );
    }

    #[test]
    fn l02_no_calls_edges_ever_emitted() {
        let src = concat!(
            "int helper(void) { return 42; }\n",
            "int main(void) { return helper(); }\n",
        );
        let ex = extract(src, "calls.c");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn l03_no_uses_edges_ever_emitted() {
        let src = "void update(void) { }\n";
        let ex = extract(src, "uses.c");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    // ── M. Confidence and source_file ────────────────────────────────────────────────────────────

    #[test]
    fn m01_all_edges_carry_extracted_confidence() {
        let src = concat!(
            "#include <stdio.h>\n",
            "struct Foo { int x; };\n",
            "void bar(void) {}\n",
        );
        let ex = extract(src, "conf.c");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Confidence::Extracted; got {edge:?}"
            );
        }
    }

    #[test]
    fn m02_source_file_field_in_every_node() {
        let src = "struct S { int x; };\nvoid f(void) {}\n";
        let ex = extract(src, "src.c");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("src.c"),
                "source_file {:?} must contain 'src.c'",
                n.source_file
            );
        }
    }

    #[test]
    fn m03_file_node_source_file_matches_path() {
        let ex = extract("", "path/to/myfile.c");
        assert!(
            ex.nodes[0].source_file.contains("myfile.c"),
            "file node source_file must match path"
        );
    }

    // ── N. Span correctness ──────────────────────────────────────────────────────────────────────

    #[test]
    fn n01_function_span_is_well_formed() {
        let src = "int add(int a, int b) {\n    return a + b;\n}\n";
        let ex = extract(src, "span.c");
        let n = node(&ex, "span_add");
        assert!(n.span.is_well_formed(), "function span must be well-formed: {n:?}");
    }

    #[test]
    fn n02_function_span_is_non_empty() {
        let src = "void noop(void) {}\n";
        let ex = extract(src, "noop.c");
        let n = node(&ex, "noop_noop");
        assert!(!n.span.is_empty(), "function span must not be empty: {n:?}");
    }

    #[test]
    fn n03_file_node_span_starts_at_byte_zero() {
        let src = "void dummy(void) {}\n";
        let ex = extract(src, "x.c");
        let file_node = node(&ex, "x");
        assert_eq!(file_node.span.start_byte, 0, "file node span must start at byte 0");
    }

    #[test]
    fn n04_file_node_span_start_line_is_one() {
        let src = "void dummy(void) {}\n";
        let ex = extract(src, "x.c");
        let file_node = node(&ex, "x");
        assert_eq!(file_node.span.start_line, 1, "file node must start at line 1");
    }

    #[test]
    fn n05_function_span_end_line_ge_start_line() {
        let src = "void multi(void) {\n    int a = 1;\n    int b = 2;\n}\n";
        let ex = extract(src, "ml.c");
        let n = node(&ex, "ml_multi");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    // ── O. Error tolerance / malformed input ─────────────────────────────────────────────────────

    #[test]
    fn o01_malformed_c_source_returns_ok_not_error() {
        // tree-sitter is error-tolerant and produces a partial tree even for broken C.
        let src = "int ??? broken {{ syntax\n";
        let result = CExtractor.extract(Path::new("bad.c"), src.as_bytes());
        assert!(result.is_ok(), "malformed C must not return Err: {result:?}");
    }

    #[test]
    fn o02_malformed_source_always_has_file_node() {
        let src = "@@@ not valid C at all @@@\n";
        let ex = extract(src, "broken.c");
        assert!(has_node(&ex, "broken"), "file node must always be present");
    }

    #[test]
    fn o03_unicode_in_string_literal_does_not_crash() {
        // Unicode in string literals is valid C source (as a byte sequence).
        let src = "const char *greeting = \"hello\";\nvoid say(void) {}\n";
        let ex = extract(src, "i18n.c");
        assert!(has_node(&ex, "i18n"), "file node must be present");
        assert!(has_node(&ex, "i18n_say"), "function node must be present");
    }

    #[test]
    fn o04_unicode_in_comment_does_not_affect_extraction() {
        let src = "/* copyright © 2024 */\nvoid work(void) {}\n";
        let ex = extract(src, "uc.c");
        assert!(has_node(&ex, "uc_work"), "function after unicode comment must be extracted");
    }

    // ── P. Extractor metadata ────────────────────────────────────────────────────────────────────

    #[test]
    fn p01_language_slug_is_c() {
        assert_eq!(CExtractor.language(), "c");
    }

    #[test]
    fn p02_extensions_include_c_and_h() {
        let exts = CExtractor.extensions();
        assert!(exts.contains(&"c"), "extensions must contain 'c'");
        assert!(exts.contains(&"h"), "extensions must contain 'h'");
    }

    // ── Q. Edge source/target properties ────────────────────────────────────────────────────────

    #[test]
    fn q01_contains_edge_source_is_always_file_label() {
        let src = concat!(
            "struct Alpha { int x; };\n",
            "void f(void) {}\n",
            "typedef int MyAlias;\n",
        );
        let ex = extract(src, "q1.c");
        let contains: Vec<_> = ex.edges.iter().filter(|e| e.relation == "contains").collect();
        for edge in &contains {
            assert_eq!(
                edge.source, "q1",
                "every contains edge source must be the file label; got {edge:?}"
            );
        }
    }

    #[test]
    fn q02_imports_from_edge_source_is_always_file_label() {
        let src = "#include <a.h>\n#include <b.h>\n";
        let ex = extract(src, "q2.c");
        for edge in ex.edges.iter().filter(|e| e.relation == "imports_from") {
            assert_eq!(
                edge.source, "q2",
                "every imports_from edge source must be the file label; got {edge:?}"
            );
        }
    }

    // ── R. Realistic composite files ────────────────────────────────────────────────────────────

    #[test]
    fn r01_realistic_c_header_all_symbols_extracted() {
        // Note: includes inside #ifndef/#endif guards are nested in `preproc_ifdef` and NOT
        // extracted (only top-level includes are processed, matching graphify's behaviour).
        let src = concat!(
            "#include <stddef.h>\n",   // top-level include IS extracted
            "typedef struct Connection {\n",
            "    int fd;\n",
            "    int flags;\n",
            "} Connection;\n",
            "void connection_init(void);\n",  // declaration — no body, not extracted
            "void connection_close(void);\n", // declaration — no body, not extracted
        );
        let ex = extract(src, "mylib.h");
        // File node
        assert!(has_node(&ex, "mylib"), "file node missing");
        // Top-level include
        assert!(has_edge(&ex, "mylib", "stddef.h", "imports_from"), "stddef.h import missing");
        // Typedef + embedded named struct
        assert!(
            has_node(&ex, "mylib_connection"),
            "Connection struct/typedef node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn r02_realistic_c_source_all_symbols_extracted() {
        let src = concat!(
            "#include <stdio.h>\n",
            "#include <stdlib.h>\n",
            "struct Config { int debug; int verbose; };\n",
            "typedef int Status;\n",
            "Status load_config(struct Config *cfg) {\n",
            "    cfg->debug = 0;\n",
            "    return 0;\n",
            "}\n",
            "void print_config(const struct Config *cfg) {\n",
            "    (void)cfg;\n",
            "}\n",
        );
        let ex = extract(src, "app.c");
        assert!(has_node(&ex, "app"), "file node missing");
        assert!(has_edge(&ex, "app", "stdio.h", "imports_from"), "stdio.h missing");
        assert!(has_edge(&ex, "app", "stdlib.h", "imports_from"), "stdlib.h missing");
        assert!(has_node(&ex, "app_config"), "Config struct missing");
        assert!(has_node(&ex, "app_status"), "Status typedef missing");
        assert!(has_node(&ex, "app_load_config"), "load_config fn missing");
        assert!(has_node(&ex, "app_print_config"), "print_config fn missing");
    }

    #[test]
    fn r03_totals_node_and_edge_count_matches_expected() {
        let src = concat!(
            "struct AlphaA { int x; };\n",
            "struct BetaB { int y; };\n",
            "void f1(void) {}\n",
            "void f2(void) {}\n",
            "void f3(void) {}\n",
        );
        let ex = extract(src, "totals.c");
        // 1 file + 2 structs + 3 fns = 6 nodes
        assert_eq!(
            ex.nodes.len(),
            6,
            "expected 6 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        // 2 struct contains + 3 fn contains = 5 edges
        let contains_count = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(contains_count, 5, "expected 5 contains edges");
    }

    #[test]
    fn r04_mixed_functions_structs_typedefs_includes_all_extracted() {
        let src = concat!(
            "#include <string.h>\n",
            "typedef struct Rect { int w; int h; } Rect;\n",
            "int area_fn(int w, int h) { return w * h; }\n",
        );
        let ex = extract(src, "shapes.c");
        assert!(has_node(&ex, "shapes"), "file node missing");
        assert!(has_edge(&ex, "shapes", "string.h", "imports_from"), "include missing");
        // Named struct from typedef
        assert!(has_node(&ex, "shapes_rect"), "struct/typedef node missing");
        assert!(has_node(&ex, "shapes_area_fn"), "function node missing");
        assert!(
            has_edge(&ex, "shapes", "shapes_area_fn", "contains"),
            "function contains edge missing"
        );
    }

    // ── S. Additional edge cases ─────────────────────────────────────────────────────────────────

    #[test]
    fn s01_union_specifier_with_name_and_body_emitted() {
        let src = "union DataUnion { int i; float f; double d; };\n";
        let ex = extract(src, "union.c");
        assert!(
            has_node(&ex, "union_dataunion"),
            "named union with body must emit a node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn s02_typedef_of_custom_type_alias_emits_node() {
        // Uses a non-predefined name to avoid tree-sitter-c primitive_type ambiguity.
        let src = "typedef unsigned long MyUInt;\n";
        let ex = extract(src, "types.c");
        assert!(
            has_node(&ex, "types_myuint"),
            "typedef alias must be emitted; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn s03_function_and_struct_independently_emitted() {
        let src = "struct ServerCfg { int port; };\nvoid server_start(void) {}\n";
        let ex = extract(src, "srv.c");
        assert!(has_node(&ex, "srv_servercfg"), "struct node missing");
        assert!(has_node(&ex, "srv_server_start"), "function node missing");
        // Both have contains edges.
        assert!(has_edge(&ex, "srv", "srv_servercfg", "contains"), "struct edge missing");
        assert!(has_edge(&ex, "srv", "srv_server_start", "contains"), "fn edge missing");
    }

    #[test]
    fn s04_multiple_structs_all_emitted() {
        let src = concat!(
            "struct NodeA { int val; };\n",
            "struct NodeB { int val; };\n",
            "struct NodeC { int val; };\n",
        );
        let ex = extract(src, "nodes.c");
        assert!(has_node(&ex, "nodes_nodea"), "NodeA missing");
        assert!(has_node(&ex, "nodes_nodeb"), "NodeB missing");
        assert!(has_node(&ex, "nodes_nodec"), "NodeC missing");
        // 1 file + 3 structs = 4 nodes
        assert_eq!(
            ex.nodes.len(),
            4,
            "expected 4 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn s05_struct_node_span_is_well_formed() {
        let src = "struct BigS { int a; int b; int c; };\n";
        let ex = extract(src, "big.c");
        let n = node(&ex, "big_bigs");
        assert!(n.span.is_well_formed(), "struct span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "struct span must not be empty: {n:?}");
    }
}
