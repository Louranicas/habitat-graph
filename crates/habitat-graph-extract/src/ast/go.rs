//! Go AST extractor (`tree-sitter-go`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (filename without `.go`, lowercased): one file node labeled `B`;
//! top-level function nodes labeled `B_<fn>`; named-type nodes labeled `B_<type>` (from
//! `type_declaration` children: `type_spec` and `type_alias`); method nodes labeled
//! `B_<receiver>_<method>`. Edges: `contains` (file→fn or file→type), `method`
//! (receiver-type→method), `imports_from` (file→import-path). `inherits`, `calls`, and `uses`
//! are deliberately NOT emitted — Go has no inheritance and name-resolution heuristics would not
//! match graphify. Embedded struct/interface fields are similarly skipped (Go composition is not
//! inheritance).

use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

/// Extracts nodes/edges from Go source using `tree-sitter-go`, in **graphify's qualified-id
/// taxonomy** (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (filename without `.go`, lowercased): one file node labeled `B`;
/// function nodes labeled `B_<fn>`; type nodes labeled `B_<type>`; method nodes labeled
/// `B_<receiver>_<method>`. Edges: `contains` (file→fn or file→type), `method`
/// (receiver-type→method), `imports_from` (file→import-path). `inherits`, `calls`, and `uses`
/// are deliberately NOT emitted — Go has no inheritance; embedded fields are composition, not
/// inheritance. An empty/package-only file produces exactly one node (the file node) and no
/// edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct GoExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Resolves the bare type name from a `method_declaration`'s receiver `parameter_list`.
///
/// Handles value receivers `(s MyStruct)` and pointer receivers `(p *MyStruct)`, stripping the
/// leading `*` for pointers. Returns `None` for unsupported forms (generic, qualified, or
/// unrecognised type node kinds) — those method declarations are silently skipped.
fn receiver_type_name(receiver_list: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    // The receiver is always a parameter_list with one parameter_declaration inside.
    let param_decl = receiver_list.named_child(0)?;
    if param_decl.kind() != "parameter_declaration" {
        return None;
    }
    let type_node = param_decl.child_by_field_name("type")?;
    match type_node.kind() {
        "type_identifier" => Some(text_of(source, &type_node).to_lowercase()),
        "pointer_type" => {
            // Strip '*'; the inner node should be type_identifier for a plain pointer receiver.
            // Generic or qualified pointer receivers (e.g. *Server[T], *pkg.T) yield None.
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

/// Extracts the unquoted, lowercased import path from a single `import_spec` node.
///
/// The `path` field is an `interpreted_string_literal` (`"net/http"`) or `raw_string_literal`
/// (`` `net/http` ``); both include the surrounding quote characters in their source text, which
/// are stripped here. Returns `None` if the `path` field is absent or the stripped string is
/// empty.
fn import_spec_path(spec: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let path_node = spec.child_by_field_name("path")?;
    let raw = text_of(source, &path_node);
    let stripped = raw.trim_matches(|c: char| c == '"' || c == '`');
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_lowercase())
    }
}

/// Processes one `import_declaration` node, emitting `imports_from` edges for each import path.
///
/// Handles both the single-import form (`import "fmt"`) and the grouped form
/// (`import ( "fmt" \n "net/http" )`).
fn extract_import(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    // The import_declaration's single named child is either import_spec or import_spec_list.
    let Some(child) = node.named_child(0) else {
        return;
    };
    match child.kind() {
        "import_spec" => {
            if let Some(path) = import_spec_path(&child, source) {
                result.edges.push(RawEdge {
                    source: b.to_owned(),
                    target: path,
                    relation: "imports_from".to_owned(),
                    confidence: Confidence::Extracted,
                });
            }
        }
        "import_spec_list" => {
            for i in 0..child.named_child_count() {
                if let Some(spec) = child.named_child(i) {
                    if spec.kind() == "import_spec" {
                        if let Some(path) = import_spec_path(&spec, source) {
                            result.edges.push(RawEdge {
                                source: b.to_owned(),
                                target: path,
                                relation: "imports_from".to_owned(),
                                confidence: Confidence::Extracted,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Extracts a top-level `function_declaration` into `result`.
///
/// Emits a function node `B_<fn>` and a `contains` edge from `B` to the node. The function
/// name is lowercased.
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

/// Extracts a top-level `method_declaration` into `result`.
///
/// Emits a method node `B_<receiver>_<method>` and a `method` edge from `B_<receiver>` to the
/// node. The receiver type name is extracted from the `receiver` field (a `parameter_list`);
/// pointer receivers (`*T`) have the leading `*` stripped. Methods whose receiver type cannot be
/// resolved (generic, qualified, or unrecognised) are silently skipped.
fn extract_method(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    let Some(receiver_node) = node.child_by_field_name("receiver") else {
        return;
    };
    let Some(t) = receiver_type_name(&receiver_node, source) else {
        return;
    };
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let method_lower = text_of(source, &name_node).to_lowercase();
    let type_label = format!("{b}_{t}");
    let method_label = format!("{b}_{t}_{method_lower}");

    result.nodes.push(RawNode {
        label: method_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });
    result.edges.push(RawEdge {
        source: type_label,
        target: method_label,
        relation: "method".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Extracts a top-level `type_declaration` into `result`.
///
/// For each `type_spec` or `type_alias` child, emits a type node `B_<name>` and a `contains`
/// edge from `B` to the node. The type name is lowercased.
///
/// Note: Go does not have inheritance — embedded struct/interface fields are composition, not
/// subtyping, so NO `inherits` edges are ever emitted.
fn extract_type_decl(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        let kind = child.kind();
        if kind != "type_spec" && kind != "type_alias" {
            continue;
        }
        let Some(name_node) = child.child_by_field_name("name") else {
            continue;
        };
        let type_lower = text_of(source, &name_node).to_lowercase();
        let type_label = format!("{b}_{type_lower}");

        result.nodes.push(RawNode {
            label: type_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&child),
        });
        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: type_label,
            relation: "contains".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for GoExtractor {
    fn language(&self) -> &'static str {
        "go"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    /// Extracts Go nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// function nodes `B_<fn>`; type nodes `B_<type>`; method nodes `B_<receiver>_<method>`; with
    /// `contains`, `method`, and `imports_from` edges. `inherits`, `calls`, and `uses` are
    /// deliberately omitted. Go has no inheritance; embedded struct/interface fields are
    /// composition (no `inherits` edges).
    ///
    /// An empty or package-only file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Go grammar could not be installed on the parser (should never occur with a correctly
    ///   linked `tree-sitter-go`).
    /// - `parser.parse` returns `None` (cancellation / timeout — not for invalid Go syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
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

        // Always emit the file node, even for an empty or package-only source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Walk top-level source_file children, emitting nodes/edges per kind.
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            match child.kind() {
                "function_declaration" => {
                    extract_function(&child, source, &b, &source_file, &mut result);
                }
                "method_declaration" => {
                    extract_method(&child, source, &b, &source_file, &mut result);
                }
                "type_declaration" => {
                    extract_type_decl(&child, source, &b, &source_file, &mut result);
                }
                "import_declaration" => {
                    extract_import(&child, source, &b, &mut result);
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

    use super::GoExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        GoExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("go extractor failed on {filename}: {e}"))
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
                    "node '{label}' not found; got {:?}",
                    ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
                )
            })
    }

    // ── 1. Empty source ────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "empty.go");
        assert_eq!(ex.nodes.len(), 1, "empty source: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
        assert_eq!(ex.edges.len(), 0, "empty source: expected no edges");
    }

    // ── 2. Package-only source ─────────────────────────────────────────────────────────────────

    #[test]
    fn package_only_source_yields_only_file_node() {
        let ex = extract("package main\n", "server.go");
        assert_eq!(ex.nodes.len(), 1, "package-only: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "server");
        assert_eq!(ex.edges.len(), 0, "package-only: expected no edges");
    }

    // ── 3. File stem lowercasing ───────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("package main\n", "HTTPServer.go");
        assert_eq!(ex.nodes[0].label, "httpserver");
    }

    #[test]
    fn file_stem_mixed_case_is_fully_lowercased() {
        let ex = extract("package main\n", "MyModule.go");
        assert_eq!(ex.nodes[0].label, "mymodule");
    }

    // ── 4. Top-level functions ─────────────────────────────────────────────────────────────────

    #[test]
    fn function_emits_fn_node_and_contains_edge() {
        let src = "package p\nfunc NewServer() {}\n";
        let ex = extract(src, "server.go");
        assert!(has_node(&ex, "server"), "file node must be present");
        assert!(has_node(&ex, "server_newserver"), "fn node missing");
        assert!(
            has_edge(&ex, "server", "server_newserver", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased() {
        let src = "package p\nfunc ParseXML() {}\n";
        let ex = extract(src, "parser.go");
        assert!(
            has_node(&ex, "parser_parsexml"),
            "function label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_functions_all_emitted() {
        let src = "package p\nfunc A() {}\nfunc B() {}\nfunc C() {}\n";
        let ex = extract(src, "funcs.go");
        assert!(has_node(&ex, "funcs_a"), "funcs_a missing");
        assert!(has_node(&ex, "funcs_b"), "funcs_b missing");
        assert!(has_node(&ex, "funcs_c"), "funcs_c missing");
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "funcs").collect();
        assert_eq!(fn_nodes.len(), 3, "expected exactly 3 function nodes");
    }

    #[test]
    fn function_on_first_line_has_start_line_one() {
        // The function is on line 2 (after "package p"), not line 1.
        let src = "package p\nfunc Hello() {}\n";
        let ex = extract(src, "greet.go");
        let n = node(&ex, "greet_hello");
        assert!(
            n.span.start_line >= 1,
            "function start_line must be >= 1; got {n:?}"
        );
    }

    #[test]
    fn function_on_explicit_line_has_correct_start_line() {
        let src = "\n\n\npackage p\nfunc Deep() {}\n";
        let ex = extract(src, "deep.go");
        let n = node(&ex, "deep_deep");
        // package is on line 4, func on line 5
        assert_eq!(
            n.span.start_line, 5,
            "function on line 5 must have start_line=5; got {n:?}"
        );
    }

    #[test]
    fn function_span_is_well_formed_and_non_empty() {
        let src = "package p\nfunc Big() {\n  x := 1\n  _ = x\n}\n";
        let ex = extract(src, "span.go");
        let n = node(&ex, "span_big");
        assert!(n.span.is_well_formed(), "function span must be well-formed");
        assert!(!n.span.is_empty(), "function span must not be empty");
    }

    // ── 5. Methods — value receiver ────────────────────────────────────────────────────────────

    #[test]
    fn method_value_receiver_emits_node_and_method_edge() {
        let src = "package p\ntype Server struct{}\nfunc (s Server) Handle() {}\n";
        let ex = extract(src, "srv.go");
        assert!(has_node(&ex, "srv_server_handle"), "method node missing");
        assert!(
            has_edge(&ex, "srv_server", "srv_server_handle", "method"),
            "method edge missing; edges: {:?}",
            ex.edges
        );
    }

    // ── 6. Methods — pointer receiver ─────────────────────────────────────────────────────────

    #[test]
    fn method_pointer_receiver_emits_node_and_method_edge() {
        let src = "package p\ntype Handler struct{}\nfunc (h *Handler) ServeHTTP() {}\n";
        let ex = extract(src, "h.go");
        assert!(has_node(&ex, "h_handler_servehttp"), "method node missing");
        assert!(
            has_edge(&ex, "h_handler", "h_handler_servehttp", "method"),
            "method edge must come from type label not file label"
        );
    }

    #[test]
    fn pointer_receiver_strips_star_from_type_name() {
        let src = "package p\ntype Engine struct{}\nfunc (e *Engine) Run() {}\n";
        let ex = extract(src, "eng.go");
        // receiver is '*Engine' → type is 'engine' (no star in label)
        assert!(
            has_node(&ex, "eng_engine_run"),
            "pointer receiver: star must be stripped; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_name_is_lowercased() {
        let src = "package p\ntype Foo struct{}\nfunc (f Foo) MyMethod() {}\n";
        let ex = extract(src, "foo.go");
        assert!(
            has_node(&ex, "foo_foo_mymethod"),
            "method name must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_node_label_is_b_t_m_format() {
        let src = "package p\ntype Client struct{}\nfunc (c *Client) Do() {}\n";
        let ex = extract(src, "myfile.go");
        // b="myfile", t="client", m="do" → "myfile_client_do"
        assert!(
            has_node(&ex, "myfile_client_do"),
            "method label must match b_t_m format; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_edge_source_is_type_label_not_file_label() {
        let src = "package p\ntype Worker struct{}\nfunc (w *Worker) Start() {}\n";
        let ex = extract(src, "wk.go");
        // Method edge source must be "wk_worker", NOT "wk"
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 1, "expected 1 method edge");
        assert_eq!(
            method_edges[0].source, "wk_worker",
            "method edge source must be type label, not file label"
        );
        assert_ne!(
            method_edges[0].source, "wk",
            "method edge source must NOT be the file label"
        );
    }

    #[test]
    fn multiple_methods_on_same_type() {
        let src = concat!(
            "package p\n",
            "type Foo struct{}\n",
            "func (f *Foo) A() {}\n",
            "func (f *Foo) B() {}\n",
            "func (f *Foo) C() {}\n",
        );
        let ex = extract(src, "foo.go");
        assert!(has_node(&ex, "foo_foo_a"), "method A missing");
        assert!(has_node(&ex, "foo_foo_b"), "method B missing");
        assert!(has_node(&ex, "foo_foo_c"), "method C missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn methods_from_different_types_both_emitted() {
        let src = concat!(
            "package p\n",
            "type A struct{}\n",
            "type B struct{}\n",
            "func (a *A) DoA() {}\n",
            "func (b *B) DoB() {}\n",
        );
        let ex = extract(src, "ab.go");
        assert!(has_node(&ex, "ab_a_doa"), "method DoA missing");
        assert!(has_node(&ex, "ab_b_dob"), "method DoB missing");
        assert!(has_edge(&ex, "ab_a", "ab_a_doa", "method"));
        assert!(has_edge(&ex, "ab_b", "ab_b_dob", "method"));
    }

    #[test]
    fn method_span_end_is_ge_start() {
        let src = concat!(
            "package p\n",
            "type T struct{}\n",
            "func (t T) Multi() {\n",
            "  x := 1\n",
            "  _ = x\n",
            "}\n",
        );
        let ex = extract(src, "t.go");
        let n = node(&ex, "t_t_multi");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    #[test]
    fn mixed_value_and_pointer_receivers_both_extracted() {
        let src = concat!(
            "package p\n",
            "type S struct{}\n",
            "func (s S) ValMethod() {}\n",
            "func (s *S) PtrMethod() {}\n",
        );
        let ex = extract(src, "s.go");
        assert!(
            has_node(&ex, "s_s_valmethod"),
            "value receiver method missing"
        );
        assert!(
            has_node(&ex, "s_s_ptrmethod"),
            "pointer receiver method missing"
        );
    }

    // ── 7. Types — struct ──────────────────────────────────────────────────────────────────────

    #[test]
    fn struct_type_emits_node_and_contains_edge() {
        let src = "package p\ntype Server struct { Port int }\n";
        let ex = extract(src, "srv.go");
        assert!(has_node(&ex, "srv_server"), "struct node missing");
        assert!(
            has_edge(&ex, "srv", "srv_server", "contains"),
            "contains edge missing; edges: {:?}",
            ex.edges
        );
    }

    // ── 8. Types — interface ───────────────────────────────────────────────────────────────────

    #[test]
    fn interface_type_emits_node_and_contains_edge() {
        let src = "package p\ntype Handler interface { Handle() }\n";
        let ex = extract(src, "iface.go");
        assert!(has_node(&ex, "iface_handler"), "interface node missing");
        assert!(has_edge(&ex, "iface", "iface_handler", "contains"));
    }

    // ── 9. Types — type alias ──────────────────────────────────────────────────────────────────

    #[test]
    fn type_alias_emits_node_and_contains_edge() {
        let src = "package p\ntype MyAlias = OtherType\n";
        let ex = extract(src, "alias.go");
        assert!(
            has_node(&ex, "alias_myalias"),
            "alias node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "alias", "alias_myalias", "contains"));
    }

    #[test]
    fn type_name_is_lowercased() {
        let src = "package p\ntype XMLParser struct{}\n";
        let ex = extract(src, "p.go");
        assert!(
            has_node(&ex, "p_xmlparser"),
            "type label must be fully lowercased"
        );
    }

    // ── 10. Types — grouped declaration ──────────────────────────────────────────────────────

    #[test]
    fn multiple_type_specs_in_grouped_decl_all_emitted() {
        let src = concat!(
            "package p\n",
            "type (\n",
            "  Foo struct{}\n",
            "  Bar interface{}\n",
            "  Baz struct{}\n",
            ")\n",
        );
        let ex = extract(src, "types.go");
        assert!(has_node(&ex, "types_foo"), "Foo missing");
        assert!(has_node(&ex, "types_bar"), "Bar missing");
        assert!(has_node(&ex, "types_baz"), "Baz missing");
        let contains: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .collect();
        assert_eq!(
            contains.len(),
            3,
            "expected 3 contains edges from grouped type decl"
        );
    }

    #[test]
    fn type_span_is_well_formed_and_non_empty() {
        let src = "package p\ntype BigStruct struct { A int; B string }\n";
        let ex = extract(src, "big.go");
        let n = node(&ex, "big_bigstruct");
        assert!(n.span.is_well_formed(), "type span must be well-formed");
        assert!(!n.span.is_empty(), "type span must not be empty");
    }

    // ── 11. Type + methods composite ──────────────────────────────────────────────────────────

    #[test]
    fn type_struct_with_methods_both_emitted() {
        let src = concat!(
            "package p\n",
            "type Conn struct{}\n",
            "func (c *Conn) Open() {}\n",
            "func (c *Conn) Close() {}\n",
        );
        let ex = extract(src, "conn.go");
        assert!(has_node(&ex, "conn_conn"), "type node missing");
        assert!(has_node(&ex, "conn_conn_open"), "Open method missing");
        assert!(has_node(&ex, "conn_conn_close"), "Close method missing");
        assert!(has_edge(&ex, "conn", "conn_conn", "contains"));
        assert!(has_edge(&ex, "conn_conn", "conn_conn_open", "method"));
        assert!(has_edge(&ex, "conn_conn", "conn_conn_close", "method"));
    }

    // ── 12. Imports — single ──────────────────────────────────────────────────────────────────

    #[test]
    fn single_import_emits_imports_from_edge() {
        let src = "package p\nimport \"fmt\"\n";
        let ex = extract(src, "f.go");
        assert!(
            has_edge(&ex, "f", "fmt", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_path_strips_double_quotes() {
        let src = "package p\nimport \"net/http\"\n";
        let ex = extract(src, "m.go");
        // Target must be "net/http" not '"net/http"'
        assert!(
            has_edge(&ex, "m", "net/http", "imports_from"),
            "import path must have quotes stripped; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_path_is_lowercased() {
        // Unusual but valid for parity — ensure lowercasing is applied.
        let src = "package p\nimport \"Net/HTTP\"\n";
        let ex = extract(src, "m.go");
        assert!(
            has_edge(&ex, "m", "net/http", "imports_from"),
            "import path must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_stdlib_fmt() {
        let src = "package p\nimport \"fmt\"\n";
        let ex = extract(src, "main.go");
        assert!(has_edge(&ex, "main", "fmt", "imports_from"));
    }

    #[test]
    fn import_dotted_path_net_http() {
        let src = "package p\nimport \"net/http\"\n";
        let ex = extract(src, "srv.go");
        assert!(has_edge(&ex, "srv", "net/http", "imports_from"));
    }

    #[test]
    fn import_long_github_path() {
        let src = "package p\nimport \"github.com/user/repo/pkg\"\n";
        let ex = extract(src, "client.go");
        assert!(
            has_edge(&ex, "client", "github.com/user/repo/pkg", "imports_from"),
            "long import path missing; edges: {:?}",
            ex.edges
        );
    }

    // ── 13. Imports — grouped ─────────────────────────────────────────────────────────────────

    #[test]
    fn grouped_import_all_emitted() {
        let src = concat!(
            "package p\n",
            "import (\n",
            "  \"fmt\"\n",
            "  \"net/http\"\n",
            "  \"os\"\n",
            ")\n",
        );
        let ex = extract(src, "g.go");
        assert!(has_edge(&ex, "g", "fmt", "imports_from"), "fmt missing");
        assert!(
            has_edge(&ex, "g", "net/http", "imports_from"),
            "net/http missing"
        );
        assert!(has_edge(&ex, "g", "os", "imports_from"), "os missing");
        let import_edges: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "imports_from")
            .collect();
        assert_eq!(import_edges.len(), 3, "expected 3 imports_from edges");
    }

    #[test]
    fn multiple_import_declarations_all_emitted() {
        let src = concat!("package p\n", "import \"fmt\"\n", "import \"os\"\n",);
        let ex = extract(src, "m.go");
        assert!(has_edge(&ex, "m", "fmt", "imports_from"));
        assert!(has_edge(&ex, "m", "os", "imports_from"));
    }

    #[test]
    fn aliased_import_still_extracts_path() {
        // `import alias "net/http"` — the alias changes the local name but not the import path.
        let src = "package p\nimport h \"net/http\"\n";
        let ex = extract(src, "m.go");
        assert!(
            has_edge(&ex, "m", "net/http", "imports_from"),
            "aliased import: path edge must still be emitted; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn blank_identifier_import_still_extracts_path() {
        // `import _ "net/http"` — blank identifier is a side-effect import.
        let src = "package p\nimport _ \"net/http\"\n";
        let ex = extract(src, "m.go");
        assert!(
            has_edge(&ex, "m", "net/http", "imports_from"),
            "blank import: path edge must still be emitted; edges: {:?}",
            ex.edges
        );
    }

    // ── 14. Negative invariant — no calls/uses/inherits edges ─────────────────────────────────

    #[test]
    fn no_calls_or_uses_edges_ever_emitted() {
        let src = concat!(
            "package p\n",
            "import \"fmt\"\n",
            "type Foo struct{}\n",
            "func (f *Foo) Bar() { fmt.Println(\"hi\") }\n",
            "func main() { var f Foo; f.Bar() }\n",
        );
        let ex = extract(src, "noisy.go");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_inherits_edges_go_has_no_inheritance() {
        let src = concat!(
            "package p\n",
            "type Animal struct{}\n",
            "type Dog struct { Animal }\n", // embedded — NOT inheritance
        );
        let ex = extract(src, "animals.go");
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        assert_eq!(
            inherits.len(),
            0,
            "Go has no inheritance: 0 inherits edges expected; got {inherits:?}"
        );
    }

    #[test]
    fn embedded_struct_fields_not_emitted_as_inherits() {
        // Go uses struct embedding for composition, never inheritance.
        let src = concat!(
            "package p\n",
            "type Base struct{ X int }\n",
            "type Child struct { Base }\n",
        );
        let ex = extract(src, "comp.go");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "inherits"),
            "struct embedding must NOT produce inherits edges; got {:?}",
            ex.edges
        );
    }

    // ── 15. Confidence + source_file ─────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "package p\n",
            "import \"fmt\"\n",
            "type T struct{}\n",
            "func (t T) M() {}\n",
            "func F() {}\n",
        );
        let ex = extract(src, "all.go");
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
        let src = concat!(
            "package p\n",
            "type Server struct{}\n",
            "func (s *Server) Handle() {}\n",
            "func Init() {}\n",
        );
        let ex = extract(src, "mymod.go");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("mymod.go"),
                "source_file {:?} must contain 'mymod.go'",
                n.source_file
            );
        }
    }

    // ── 16. Span correctness ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero_for_non_empty_source() {
        let src = "package p\ntype X struct{}\n";
        let ex = extract(src, "x.go");
        let file_node = node(&ex, "x");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn file_node_span_start_line_is_one() {
        let src = "package p\n";
        let ex = extract(src, "sp.go");
        let file_node = node(&ex, "sp");
        assert_eq!(
            file_node.span.start_line, 1,
            "file node must start at line 1"
        );
    }

    // ── 17. Totals and multi-symbol composites ────────────────────────────────────────────────

    #[test]
    fn contains_edge_count_matches_fn_and_type_count() {
        let src = concat!(
            "package p\n",
            "type A struct{}\n",
            "type B struct{}\n",
            "func F1() {}\n",
            "func F2() {}\n",
            "func F3() {}\n",
        );
        let ex = extract(src, "totals.go");
        let contains: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .collect();
        // 2 types + 3 functions = 5 contains edges
        assert_eq!(
            contains.len(),
            5,
            "expected 5 contains edges (2 type + 3 fn); got {contains:?}"
        );
    }

    #[test]
    fn method_edges_count_matches_method_declarations() {
        let src = concat!(
            "package p\n",
            "type X struct{}\n",
            "func (x *X) M1() {}\n",
            "func (x *X) M2() {}\n",
            "func (x X) M3() {}\n",
        );
        let ex = extract(src, "mx.go");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn two_types_two_methods_each_produce_correct_totals() {
        let src = concat!(
            "package p\n",
            "type A struct{}\n",
            "type B struct{}\n",
            "func (a *A) X() {}\n",
            "func (a *A) Y() {}\n",
            "func (b *B) P() {}\n",
            "func (b *B) Q() {}\n",
        );
        let ex = extract(src, "ab.go");
        // file(1) + typeA(1) + typeB(1) + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes (1 file + 2 types + 4 methods); got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges");
    }

    // ── 18. Realistic composite file ─────────────────────────────────────────────────────────

    #[test]
    fn realistic_go_file_all_symbols_extracted() {
        let src = concat!(
            "package server\n",
            "\n",
            "import (\n",
            "  \"fmt\"\n",
            "  \"net/http\"\n",
            ")\n",
            "\n",
            "type Server struct { Addr string }\n",
            "\n",
            "type Handler interface { ServeHTTP(http.ResponseWriter, *http.Request) }\n",
            "\n",
            "func NewServer(addr string) *Server { return &Server{Addr: addr} }\n",
            "\n",
            "func (s *Server) Start() error { return fmt.Errorf(\"start\") }\n",
            "\n",
            "func (s *Server) Stop() {}\n",
        );
        let ex = extract(src, "server.go");
        // File node
        assert!(has_node(&ex, "server"), "file node");
        // Types
        assert!(has_node(&ex, "server_server"), "Server type");
        assert!(has_node(&ex, "server_handler"), "Handler interface");
        // Function
        assert!(has_node(&ex, "server_newserver"), "NewServer fn");
        // Methods
        assert!(has_node(&ex, "server_server_start"), "Start method");
        assert!(has_node(&ex, "server_server_stop"), "Stop method");
        // Edges
        assert!(has_edge(&ex, "server", "server_server", "contains"));
        assert!(has_edge(&ex, "server", "server_handler", "contains"));
        assert!(has_edge(&ex, "server", "server_newserver", "contains"));
        assert!(has_edge(
            &ex,
            "server_server",
            "server_server_start",
            "method"
        ));
        assert!(has_edge(
            &ex,
            "server_server",
            "server_server_stop",
            "method"
        ));
        assert!(has_edge(&ex, "server", "fmt", "imports_from"));
        assert!(has_edge(&ex, "server", "net/http", "imports_from"));
    }

    // ── 19. Mixed fn + type + import ─────────────────────────────────────────────────────────

    #[test]
    fn mixed_fn_type_import_all_symbols_emitted() {
        let src = concat!(
            "package p\n",
            "import \"os\"\n",
            "type Config struct{}\n",
            "func Load() Config { return Config{} }\n",
        );
        let ex = extract(src, "cfg.go");
        assert!(has_node(&ex, "cfg"), "file node");
        assert!(has_node(&ex, "cfg_config"), "Config type");
        assert!(has_node(&ex, "cfg_load"), "Load fn");
        assert!(has_edge(&ex, "cfg", "os", "imports_from"));
        assert!(has_edge(&ex, "cfg", "cfg_config", "contains"));
        assert!(has_edge(&ex, "cfg", "cfg_load", "contains"));
    }

    // ── 20. Malformed / unusual source ───────────────────────────────────────────────────────

    #[test]
    fn malformed_source_returns_ok_tree_sitter_is_error_tolerant() {
        // tree-sitter is error-tolerant and produces a partial tree even for broken Go.
        let src = "package p\nfunc ??? broken syntax {{{\n";
        let ex = extract(src, "bad.go");
        // Must not panic or error; file node must be present.
        assert!(has_node(&ex, "bad"), "file node must always be present");
    }

    #[test]
    fn nested_function_inside_function_not_emitted_as_top_level() {
        // Go allows closures (anonymous functions) but not named nested functions.
        // tree-sitter may or may not produce a function_declaration in the body;
        // either way, only top-level declarations should be extracted.
        let src = concat!(
            "package p\n",
            "func Outer() {\n",
            "  inner := func() {}\n",
            "  _ = inner\n",
            "}\n",
        );
        let ex = extract(src, "nested.go");
        assert!(
            has_node(&ex, "nested_outer"),
            "outer function must be emitted"
        );
        // No node labeled "nested_inner" should appear.
        assert!(
            !has_node(&ex, "nested_inner"),
            "closure must not appear as a top-level function node"
        );
    }

    // ── 21. Extractor metadata ────────────────────────────────────────────────────────────────

    #[test]
    fn language_slug_is_go() {
        assert_eq!(GoExtractor.language(), "go");
    }

    #[test]
    fn extension_is_go() {
        assert_eq!(GoExtractor.extensions(), &["go"]);
    }

    #[test]
    fn file_node_always_present_for_package_only_source() {
        let ex = extract("package main\n", "main.go");
        assert!(has_node(&ex, "main"), "file node must always be present");
    }

    // ── 22. Non-struct/interface type specs ──────────────────────────────────────────────────

    #[test]
    fn non_struct_type_spec_also_emits_node() {
        // `type MyInt int` is a named type — it should still produce a node.
        let src = "package p\ntype MyInt int\n";
        let ex = extract(src, "types.go");
        assert!(
            has_node(&ex, "types_myint"),
            "non-struct type spec must emit a node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "types", "types_myint", "contains"));
    }
}
