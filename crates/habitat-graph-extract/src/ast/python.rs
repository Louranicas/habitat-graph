//! Python AST extractor (tree-sitter-python) — emits graphify's qualified-id taxonomy for parity.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result, Span};

use crate::registry::Extractor;

/// Extracts nodes/edges from Python source using `tree-sitter-python`, in **graphify's qualified-id
/// taxonomy** (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (filename without `.py`, lowercased): one file node labeled `B`;
/// class/function nodes labeled `B_<name>`; method nodes labeled `B_<class>_<method>`. Edges:
/// `contains` (file→symbol), `method` (class→method), `inherits` (base→derived, with external base
/// nodes), `imports_from` (file→module). `calls`/`uses` are deliberately NOT emitted (they are
/// name-resolution heuristics that would not match graphify).
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonExtractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Reads the text of a tree-sitter node from the raw source bytes.
///
/// Invalid UTF-8 is replaced lossily; Python source should always be valid UTF-8, but we
/// defend against malformed inputs rather than panicking.
fn text_of(source: &[u8], node: &tree_sitter::Node<'_>) -> String {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]).into_owned()
}

/// Builds a [`Span`] from a tree-sitter node.
///
/// Tree-sitter row/col positions are 0-based; we add 1 for the 1-based line contract.
/// `u32::try_from` guards against files > 4 GiB (degenerate but safe).
fn make_span(node: &tree_sitter::Node<'_>) -> Span {
    Span::new(
        u32::try_from(node.start_byte()).unwrap_or(u32::MAX),
        u32::try_from(node.end_byte()).unwrap_or(u32::MAX),
        u32::try_from(node.start_position().row)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
        u32::try_from(node.end_position().row)
            .unwrap_or(u32::MAX)
            .saturating_add(1),
    )
}

/// Produces the method component `m` of the qualified id `B_C_m`.
///
/// Strips all leading and trailing `_` characters then lowercases.  Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting entirely of
/// underscores).
///
/// Examples: `__init__` → `"init"`, `__call__` → `"call"`, `_private` → `"private"`,
/// `myMethod` → `"mymethod"`, `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Returns the lowercased names of all top-level `class_definition` nodes in `root`.
///
/// This first pass is used to distinguish local bases (same file) from external ones when
/// emitting `inherits` edges: a local base `Foo` in file with stem `b` gets `base_id = "b_foo"`,
/// while an external base gets `base_id = "foo"`.
fn local_class_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i) {
            if child.kind() == "class_definition" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    names.insert(text_of(source, &name_node).to_lowercase());
                }
            }
        }
    }
    names
}

/// Extracts one top-level class definition into `result`.
///
/// Emits:
/// - A class node `B_c`.
/// - A `contains` edge from `B` to the class node.
/// - One `inherits` edge per simple-identifier base class (attribute/subscript bases are skipped).
/// - One method node `B_c_m` and one `method` edge per `function_definition` in the class body.
fn extract_class(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_classes: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let c = text_of(source, &name_node).to_lowercase();
    let class_label = format!("{b}_{c}");

    // Emit the class node.
    result.nodes.push(RawNode {
        label: class_label.clone(),
        source_file: source_file.to_owned(),
        span: make_span(node),
    });

    // contains: file → class
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: class_label.clone(),
        relation: "contains".to_owned(),
        confidence: Confidence::Extracted,
    });

    // inherits: base → class, one per simple-identifier base in the argument_list.
    if let Some(sc_node) = node.child_by_field_name("superclasses") {
        for i in 0..sc_node.named_child_count() {
            if let Some(base_node) = sc_node.named_child(i) {
                // Only handle plain identifiers; skip attribute (`module.Base`), subscript
                // (`Generic[T]`), keyword_argument, splats, etc.
                if base_node.kind() == "identifier" {
                    let base_lower = text_of(source, &base_node).to_lowercase();
                    let base_id = if local_classes.contains(&base_lower) {
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
        }
    }

    // Methods: walk the body block for direct function_definition children.
    if let Some(body_node) = node.child_by_field_name("body") {
        for i in 0..body_node.named_child_count() {
            if let Some(method_node) = body_node.named_child(i) {
                if method_node.kind() == "function_definition" {
                    if let Some(mname_node) = method_node.child_by_field_name("name") {
                        let raw_method = text_of(source, &mname_node);
                        let m = method_id(&raw_method);
                        let method_label = format!("{b}_{c}_{m}");

                        result.nodes.push(RawNode {
                            label: method_label.clone(),
                            source_file: source_file.to_owned(),
                            span: make_span(&method_node),
                        });

                        // method: class → method
                        result.edges.push(RawEdge {
                            source: class_label.clone(),
                            target: method_label,
                            relation: "method".to_owned(),
                            confidence: Confidence::Extracted,
                        });
                    }
                }
            }
        }
    }
}

/// Extracts one top-level function definition into `result`.
///
/// Emits a function node `B_f` and a `contains` edge from `B` to the node.
fn extract_toplevel_fn(
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

/// Extracts one `from MODULE import ...` statement into `result`.
///
/// Emits an `imports_from` edge from `B` to the lowercased module text.  Relative imports
/// (e.g. `from . import x`) are included: the `module_name` field text is used verbatim after
/// lowercasing, which yields the dotted prefix or just `"."` for a bare relative import.
fn extract_import_from(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    result: &mut Extraction,
) {
    if let Some(module_node) = node.child_by_field_name("module_name") {
        let module = text_of(source, &module_node).to_lowercase();
        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: module,
            relation: "imports_from".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

impl Extractor for PythonExtractor {
    fn language(&self) -> &'static str {
        "python"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py"]
    }

    /// Extracts Python nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased); class
    /// nodes `B_<cls>`; function nodes `B_<fn>`; method nodes `B_<cls>_<method>`; with `contains`,
    /// `method`, `inherits`, and `imports_from` edges.  `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Python grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-python`).
    /// - `parser.parse` returns `None` (cancellation / timeout — not for invalid Python syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();

        // B = lowercased file stem, e.g. "exceptions" for "exceptions.py".
        let b = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
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

        // Pass 1: collect locally-defined class names so we can classify bases as local/external.
        let local_classes = local_class_names(root, source);

        let mut result = Extraction::new();

        // Always emit the file node, even for an empty source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Pass 2: walk top-level module children, emitting nodes/edges per kind.
        for i in 0..root.named_child_count() {
            if let Some(child) = root.named_child(i) {
                match child.kind() {
                    "class_definition" => {
                        extract_class(
                            &child,
                            source,
                            &b,
                            &source_file,
                            &local_classes,
                            &mut result,
                        );
                    }
                    "function_definition" => {
                        extract_toplevel_fn(&child, source, &b, &source_file, &mut result);
                    }
                    "import_from_statement" => {
                        extract_import_from(&child, source, &b, &mut result);
                    }
                    _ => {}
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

    use super::PythonExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        PythonExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("extractor failed on {filename}: {e}"))
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
        let ex = extract("", "empty.py");
        assert_eq!(ex.nodes.len(), 1, "empty source: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
        assert_eq!(ex.edges.len(), 0, "empty source: expected no edges");
    }

    // ── 2. File stem lowercasing ────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "Exceptions.py");
        assert_eq!(ex.nodes[0].label, "exceptions");
    }

    #[test]
    fn file_stem_with_mixed_case_is_fully_lowercased() {
        let ex = extract("", "MyModule.py");
        assert_eq!(ex.nodes[0].label, "mymodule");
    }

    // ── 3. Single class, no bases ──────────────────────────────────────────────────────────────

    #[test]
    fn class_no_bases_emits_class_node_and_contains_edge_no_inherits() {
        let ex = extract("class HTTPError:\n    pass\n", "exceptions.py");
        assert!(has_node(&ex, "exceptions"), "file node must be present");
        assert!(has_node(&ex, "exceptions_httperror"), "class node missing");
        assert!(
            has_edge(&ex, "exceptions", "exceptions_httperror", "contains"),
            "contains edge missing"
        );
        let inherits_count = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(
            inherits_count, 0,
            "no bases → no inherits edges; got {inherits_count}"
        );
    }

    // ── 4. External base class ─────────────────────────────────────────────────────────────────

    #[test]
    fn external_base_yields_bare_lowercased_base_id() {
        let ex = extract("class HTTPError(Exception):\n    pass\n", "exceptions.py");
        assert!(has_node(&ex, "exceptions_httperror"), "class node missing");
        assert!(
            has_edge(&ex, "exception", "exceptions_httperror", "inherits"),
            "inherits edge with external base missing; edges: {:?}",
            ex.edges
        );
        // The contains edge must also be present.
        assert!(has_edge(
            &ex,
            "exceptions",
            "exceptions_httperror",
            "contains"
        ));
    }

    // ── 5. Local base class ────────────────────────────────────────────────────────────────────

    #[test]
    fn local_base_uses_stem_prefixed_base_id() {
        let src = "class A:\n    pass\nclass B(A):\n    pass\n";
        let ex = extract(src, "m.py");
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local base must produce stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    // ── 6. Multiple external bases ─────────────────────────────────────────────────────────────

    #[test]
    fn multiple_external_bases_each_produce_inherits_edge() {
        let src = "class C(Base1, Base2):\n    pass\n";
        let ex = extract(src, "multi.py");
        assert!(
            has_edge(&ex, "base1", "multi_c", "inherits"),
            "Base1 inherits edge missing"
        );
        assert!(
            has_edge(&ex, "base2", "multi_c", "inherits"),
            "Base2 inherits edge missing"
        );
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        assert_eq!(inherits.len(), 2, "expected exactly 2 inherits edges");
    }

    // ── 7. Dunder methods: __init__ → "init" ──────────────────────────────────────────────────

    #[test]
    fn dunder_init_strips_to_init() {
        let src = "class HTTPError:\n    def __init__(self): pass\n";
        let ex = extract(src, "exceptions.py");
        assert!(
            has_node(&ex, "exceptions_httperror_init"),
            "method node missing; nodes: {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(
            &ex,
            "exceptions_httperror",
            "exceptions_httperror_init",
            "method"
        ));
    }

    // ── 8. Dunder: __call__ → "call" ──────────────────────────────────────────────────────────

    #[test]
    fn dunder_call_strips_to_call() {
        let src = "class Callable:\n    def __call__(self): pass\n";
        let ex = extract(src, "types.py");
        assert!(
            has_node(&ex, "types_callable_call"),
            "expected types_callable_call; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 9. Plain method name is lowercased ─────────────────────────────────────────────────────

    #[test]
    fn plain_method_name_is_lowercased_in_label() {
        let src = "class Foo:\n    def MyMethod(self): pass\n";
        let ex = extract(src, "mod.py");
        assert!(
            has_node(&ex, "mod_foo_mymethod"),
            "method label should be lowercased"
        );
    }

    // ── 10. Top-level function ────────────────────────────────────────────────────────────────

    #[test]
    fn toplevel_function_emits_fn_node_and_contains_edge() {
        let src = "def helper(): pass\n";
        let ex = extract(src, "utils.py");
        assert!(has_node(&ex, "utils_helper"), "function node missing");
        assert!(
            has_edge(&ex, "utils", "utils_helper", "contains"),
            "contains edge missing"
        );
    }

    // ── 11. Multiple top-level functions ──────────────────────────────────────────────────────

    #[test]
    fn multiple_toplevel_functions_all_emitted() {
        let src = "def a(): pass\ndef b(): pass\ndef c(): pass\n";
        let ex = extract(src, "funcs.py");
        assert!(has_node(&ex, "funcs_a"), "funcs_a missing");
        assert!(has_node(&ex, "funcs_b"), "funcs_b missing");
        assert!(has_node(&ex, "funcs_c"), "funcs_c missing");
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "funcs").collect();
        assert_eq!(fn_nodes.len(), 3, "expected exactly 3 function nodes");
    }

    // ── 12. from import → imports_from edge ───────────────────────────────────────────────────

    #[test]
    fn from_import_emits_imports_from_edge() {
        let src = "from httpx import Client\n";
        let ex = extract(src, "client.py");
        assert!(
            has_edge(&ex, "client", "httpx", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    // ── 13. Dotted module import ──────────────────────────────────────────────────────────────

    #[test]
    fn dotted_module_import_is_lowercased() {
        let src = "from os.path import join\n";
        let ex = extract(src, "paths.py");
        assert!(
            has_edge(&ex, "paths", "os.path", "imports_from"),
            "dotted import edge missing"
        );
    }

    // ── 14. Multiple from imports ─────────────────────────────────────────────────────────────

    #[test]
    fn multiple_from_imports_all_emitted() {
        let src = "from os import path\nfrom sys import argv\n";
        let ex = extract(src, "imports.py");
        assert!(
            has_edge(&ex, "imports", "os", "imports_from"),
            "os edge missing"
        );
        assert!(
            has_edge(&ex, "imports", "sys", "imports_from"),
            "sys edge missing"
        );
    }

    // ── 15. Class with multiple methods ───────────────────────────────────────────────────────

    #[test]
    fn class_with_multiple_methods_emits_all_method_nodes_and_edges() {
        let src =
            "class Foo:\n    def a(self): pass\n    def b(self): pass\n    def c(self): pass\n";
        let ex = extract(src, "foo.py");
        assert!(has_node(&ex, "foo_foo_a"), "method a missing");
        assert!(has_node(&ex, "foo_foo_b"), "method b missing");
        assert!(has_node(&ex, "foo_foo_c"), "method c missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    // ── 16. Spec composite: HTTPError(Exception) + __init__ ───────────────────────────────────

    #[test]
    fn spec_example_httperror_with_exception_base_and_init() {
        let src = "class HTTPError(Exception):\n    def __init__(self): pass\n";
        let ex = extract(src, "exceptions.py");
        assert!(has_node(&ex, "exceptions"), "file node missing");
        assert!(has_node(&ex, "exceptions_httperror"), "class node missing");
        assert!(
            has_node(&ex, "exceptions_httperror_init"),
            "init method node missing"
        );
        assert!(has_edge(
            &ex,
            "exceptions",
            "exceptions_httperror",
            "contains"
        ));
        assert!(has_edge(
            &ex,
            "exception",
            "exceptions_httperror",
            "inherits"
        ));
        assert!(has_edge(
            &ex,
            "exceptions_httperror",
            "exceptions_httperror_init",
            "method"
        ));
    }

    // ── 17. All edges carry Confidence::Extracted ─────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = "class Foo(Bar):\n    def m(self): pass\n\nfrom x import y\n";
        let ex = extract(src, "f.py");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "edge {edge:?} must be Extracted"
            );
        }
    }

    // ── 18. source_file path stored in every node ─────────────────────────────────────────────

    #[test]
    fn source_file_field_contains_path_in_every_node() {
        let src = "class Foo:\n    def m(self): pass\n";
        let ex = extract(src, "mymod.py");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("mymod.py"),
                "source_file {:?} must contain 'mymod.py'",
                n.source_file
            );
        }
    }

    // ── 19. Span start_line is 1-based when definition is on the first line ───────────────────

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let src = "class Foo:\n    pass\n";
        let ex = extract(src, "x.py");
        let n = node(&ex, "x_foo");
        assert_eq!(
            n.span.start_line, 1,
            "class on line 1 must have start_line=1"
        );
    }

    // ── 20. Span start_line reflects actual line number ───────────────────────────────────────

    #[test]
    fn class_on_third_line_has_start_line_three() {
        let src = "\n\nclass Foo:\n    pass\n";
        let ex = extract(src, "y.py");
        let n = node(&ex, "y_foo");
        assert_eq!(
            n.span.start_line, 3,
            "class on line 3 must have start_line=3; got {n:?}"
        );
    }

    // ── 21. Nested class inside a class is NOT emitted as a top-level class node ───────────────

    #[test]
    fn nested_class_not_emitted_as_top_level_class_node() {
        let src = "class Outer:\n    class Inner:\n        pass\n";
        let ex = extract(src, "nested.py");
        assert!(
            has_node(&ex, "nested_outer"),
            "outer class node must be present"
        );
        assert!(
            !has_node(&ex, "nested_inner"),
            "nested class must NOT appear as a top-level class node"
        );
    }

    // ── 22. Nested function inside a function is NOT emitted as a top-level node ───────────────

    #[test]
    fn nested_function_inside_function_not_emitted() {
        let src = "def outer():\n    def inner(): pass\n";
        let ex = extract(src, "funcs.py");
        assert!(
            has_node(&ex, "funcs_outer"),
            "outer function node must be present"
        );
        assert!(
            !has_node(&ex, "funcs_inner"),
            "nested function must NOT be emitted as a top-level function node"
        );
    }

    // ── 23. Mixed class + function + import in one file ───────────────────────────────────────

    #[test]
    fn mixed_class_function_import_all_emitted() {
        let src = "from os import path\n\nclass Foo:\n    pass\n\ndef bar(): pass\n";
        let ex = extract(src, "mix.py");
        assert!(has_node(&ex, "mix"), "file node missing");
        assert!(has_node(&ex, "mix_foo"), "class node missing");
        assert!(has_node(&ex, "mix_bar"), "function node missing");
        assert!(
            has_edge(&ex, "mix", "mix_foo", "contains"),
            "class contains edge missing"
        );
        assert!(
            has_edge(&ex, "mix", "mix_bar", "contains"),
            "fn contains edge missing"
        );
        assert!(
            has_edge(&ex, "mix", "os", "imports_from"),
            "imports_from edge missing"
        );
    }

    // ── 24. Attribute base (dotted.Name) is silently skipped ─────────────────────────────────

    #[test]
    fn attribute_base_produces_no_inherits_edge() {
        let src = "class Foo(module.Base):\n    pass\n";
        let ex = extract(src, "a.py");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "inherits"),
            "attribute base must NOT produce an inherits edge; got {:?}",
            ex.edges
        );
    }

    // ── 25. Multiple top-level classes ────────────────────────────────────────────────────────

    #[test]
    fn multiple_top_level_classes_all_emitted_with_contains_edges() {
        let src = "class A:\n    pass\nclass B:\n    pass\nclass C:\n    pass\n";
        let ex = extract(src, "classes.py");
        assert!(has_node(&ex, "classes_a"));
        assert!(has_node(&ex, "classes_b"));
        assert!(has_node(&ex, "classes_c"));
        let contains: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "contains")
            .collect();
        assert_eq!(contains.len(), 3, "expected 3 contains edges");
    }

    // ── 26. Mixed local + external bases each get the correct base_id ─────────────────────────

    #[test]
    fn mixed_local_and_external_bases_classified_correctly() {
        let src = "class Base:\n    pass\nclass Child(Base, Exception):\n    pass\n";
        let ex = extract(src, "mix.py");
        // Base is local → "mix_base"
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
        // Exception is external → "exception"
        assert!(
            has_edge(&ex, "exception", "mix_child", "inherits"),
            "external base must use bare lowercased id"
        );
    }

    // ── 27. Single-underscore prefix is stripped ──────────────────────────────────────────────

    #[test]
    fn single_underscore_prefix_is_stripped_from_method_id() {
        let src = "class Foo:\n    def _private(self): pass\n";
        let ex = extract(src, "mod.py");
        // "_private".trim_matches('_') = "private"
        assert!(
            has_node(&ex, "mod_foo_private"),
            "expected mod_foo_private; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 28. Method label follows B_C_M format exactly ────────────────────────────────────────

    #[test]
    fn method_label_uses_b_underscore_c_underscore_m_format() {
        let src = "class MyClass:\n    def my_method(self): pass\n";
        let ex = extract(src, "mymod.py");
        // b="mymod", c="myclass", m="my_method" (no leading/trailing underscores in raw name)
        assert!(
            has_node(&ex, "mymod_myclass_my_method"),
            "method label must match b_c_m format; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 29. Class node span is well-formed and non-empty ─────────────────────────────────────

    #[test]
    fn class_node_span_is_well_formed_and_non_empty() {
        let src = "class Foo:\n    pass\n";
        let ex = extract(src, "x.py");
        let n = node(&ex, "x_foo");
        assert!(
            n.span.is_well_formed(),
            "class span must be well-formed: {n:?}"
        );
        assert!(!n.span.is_empty(), "class span must not be empty: {n:?}");
    }

    // ── 30. No "calls" or "uses" edges are ever emitted ──────────────────────────────────────

    #[test]
    fn no_calls_or_uses_edges_are_ever_emitted() {
        let src = "class Foo:\n    def bar(self):\n        baz()\n\ndef baz(): pass\n";
        let ex = extract(src, "noises.py");
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

    // ── 31. Method span has a valid start/end line pair ──────────────────────────────────────

    #[test]
    fn method_span_has_valid_start_end_lines() {
        let src = "class Foo:\n    def multi_line(self):\n        x = 1\n        return x\n";
        let ex = extract(src, "sl.py");
        let n = node(&ex, "sl_foo_multi_line");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    // ── 32. File node span covers the whole source range ─────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero_for_non_empty_source() {
        let src = "class Foo:\n    pass\n";
        let ex = extract(src, "span.py");
        let file_node = node(&ex, "span");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    // ── 33. Relative import: extractor must not panic ─────────────────────────────────────────

    #[test]
    fn relative_import_does_not_panic_and_file_node_present() {
        let src = "from . import x\n";
        let ex = extract(src, "pkg.py");
        assert!(has_node(&ex, "pkg"), "file node must always be present");
    }

    // ── 34. Class with only __init__ has exactly one method node ─────────────────────────────

    #[test]
    fn class_with_only_init_has_exactly_one_method_node() {
        let src = "class Widget:\n    def __init__(self): pass\n";
        let ex = extract(src, "widgets.py");
        let method_nodes: Vec<_> = ex
            .nodes
            .iter()
            .filter(|n| {
                ex.edges
                    .iter()
                    .any(|e| e.target == n.label && e.relation == "method")
            })
            .collect();
        assert_eq!(
            method_nodes.len(),
            1,
            "expected exactly 1 method node; got {method_nodes:?}"
        );
        assert_eq!(method_nodes[0].label, "widgets_widget_init");
    }

    // ── 35. Subscript base (Generic[T]) is silently skipped ──────────────────────────────────

    #[test]
    fn subscript_base_produces_no_inherits_edge() {
        let src = "from typing import Generic, TypeVar\nT = TypeVar('T')\nclass Foo(Generic[T]):\n    pass\n";
        let ex = extract(src, "gen.py");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "inherits"),
            "subscript base (Generic[T]) must not produce an inherits edge; got {:?}",
            ex.edges
        );
    }

    // ── 36. Class uppercase name lowercased correctly in label ────────────────────────────────

    #[test]
    fn class_name_is_fully_lowercased_in_label() {
        let src = "class XMLParser:\n    pass\n";
        let ex = extract(src, "parser.py");
        assert!(
            has_node(&ex, "parser_xmlparser"),
            "class label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── 37. Function name is lowercased in label ──────────────────────────────────────────────

    #[test]
    fn function_name_is_fully_lowercased_in_label() {
        let src = "def ParseXML(): pass\n";
        let ex = extract(src, "p.py");
        assert!(
            has_node(&ex, "p_parsexml"),
            "function label must be fully lowercased"
        );
    }

    // ── 38. Module import target is lowercased ────────────────────────────────────────────────

    #[test]
    fn module_import_target_is_lowercased() {
        let src = "from HTTP.Client import Session\n";
        let ex = extract(src, "c.py");
        assert!(
            has_edge(&ex, "c", "http.client", "imports_from"),
            "module target must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    // ── 39. Two classes with two methods each: totals verified ────────────────────────────────

    #[test]
    fn two_classes_two_methods_each_produce_correct_totals() {
        let src = concat!(
            "class A:\n",
            "    def x(self): pass\n",
            "    def y(self): pass\n",
            "class B:\n",
            "    def p(self): pass\n",
            "    def q(self): pass\n",
        );
        let ex = extract(src, "ab.py");
        // file + 2 classes + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes (1 file + 2 class + 4 method)"
        );
        // 2 contains + 4 method = 6 edges
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges");
    }

    // ── 40. Spec test 3 literal: m.py with class A and class B(A) ────────────────────────────

    #[test]
    fn spec_test_3_class_a_and_b_inherits_a() {
        let src = "class A:\n    pass\nclass B(A):\n    pass\n";
        let ex = extract(src, "m.py");
        // m_a → m_b inherits (local)
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local inherits edge missing"
        );
        assert!(has_node(&ex, "m_a"), "m_a node missing");
        assert!(has_node(&ex, "m_b"), "m_b node missing");
        assert!(has_node(&ex, "m"), "file node missing");
    }
}
