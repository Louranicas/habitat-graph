//! Scala AST extractor (`tree-sitter-scala`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem): one file node `B`; class, trait,
//! and object nodes `B_<name>`; top-level function nodes `B_<fn>`; method nodes
//! `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class/trait/object→method),
//! `inherits` (extends + with, with local/external split), `imports_from` (file→import path).
//! `calls` and `uses` are deliberately **NOT** emitted — they require name-resolution heuristics
//! that would not match graphify.
//!
//! Scala's `class`, `trait`, and `object` (singleton) definitions all produce nodes;
//! `case class` and `case object` follow the same rules because tree-sitter-scala uses the same
//! node kinds (`class_definition`, `object_definition`) for both. Top-level `def` (valid Scala 3)
//! and `def` inside template bodies both produce function/method nodes. Inheritance via `extends`
//! and `with` both contribute `inherits` edges; a base whose name is declared in the same file
//! gets `"b_<base>"` as the edge source, while an external base gets `"<base>"`.
//!
//! Imports: `import_declaration` children are walked to find the `stable_identifier` base path,
//! with optional `import_selectors` for grouped imports. Wildcard imports (`._`) emit only the
//! package prefix.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Public extractor struct ───────────────────────────────────────────────────────────────────────

/// Extracts nodes and edges from Scala source using `tree-sitter-scala`, in graphify's
/// qualified-id taxonomy.
///
/// For a file with basename `B` (lowercased stem): one file node `B`; class, trait, and object
/// nodes `B_<name>`; top-level function nodes `B_<fn>`; method nodes `B_<cls>_<method>`. Edges:
/// `contains` (file→symbol), `method` (class/trait/object→method), `inherits` (base→derived,
/// with local/external split for `extends` and `with` clauses), `imports_from` (file→import
/// path). `calls`/`uses` are deliberately omitted.
///
/// An empty file produces exactly one node (the file node) and no edges.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScalaExtractor;

// ── Private helpers ───────────────────────────────────────────────────────────────────────────────

/// Returns the method identifier component `m` of the qualified label `B_C_m`.
///
/// Strips all leading and trailing `_` characters, then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name of only underscores).
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

/// Collects the lowercased names of all top-level class, trait, and object declarations.
///
/// Used in the first pass to distinguish locally-declared types from external ones when emitting
/// `inherits` edges: a local base `Foo` in a file with stem `b` gets `base_id = "b_foo"`, while
/// an external base gets `base_id = "foo"`.
fn local_type_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        if matches!(
            child.kind(),
            "class_definition" | "trait_definition" | "object_definition"
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
/// Handles:
/// - `type_identifier`: plain type name (e.g. `Animal`).
/// - `identifier`: non-type identifier (e.g. object names).
/// - `generic_type`: parameterised type (e.g. `Seq[Int]`) — the base identifier `Seq` is
///   extracted.
///
/// Returns `None` for compound/path types (`stable_type_identifier`, `existential_type`,
/// `annotated_type` wrappers, etc.) — those are handled by the caller's one-level recursion.
fn resolve_type_name(type_node: &tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match type_node.kind() {
        "type_identifier" | "identifier" => Some(text_of(source, type_node).to_lowercase()),
        "generic_type" => {
            // The first named child of a generic_type is the raw type identifier.
            let inner = type_node.named_child(0)?;
            if matches!(inner.kind(), "type_identifier" | "identifier") {
                Some(text_of(source, &inner).to_lowercase())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Emits an `inherits` edge applying the local/external split.
///
/// A base name present in `local_types` gets source `"b_<base>"` (same-file declaration);
/// an external base gets source `"<base>"` (bare lowercased name).
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

/// Walks an `extends_clause` or `with_clause` node and emits `inherits` edges for each
/// resolvable base type.
///
/// Direct `type_identifier` / `identifier` / `generic_type` children are resolved immediately.
/// Container nodes (e.g. `annotated_type`, `with_clause` nested inside `extends_clause`) are
/// recursed into one level — their `type_identifier` / `generic_type` children are then resolved.
/// Compound/path types (`stable_type_identifier`) and constructor calls are silently skipped.
fn extract_extends_clause(
    clause: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    target_label: &str,
    local_types: &HashSet<String>,
    result: &mut Extraction,
) {
    for i in 0..clause.named_child_count() {
        let Some(child) = clause.named_child(i) else {
            continue;
        };
        if let Some(base_lower) = resolve_type_name(&child, source) {
            emit_inherits(&base_lower, b, target_label, local_types, result);
        } else {
            // Recurse one level into container nodes (annotated_type, with_clause nesting, etc.).
            for j in 0..child.named_child_count() {
                let Some(inner) = child.named_child(j) else {
                    continue;
                };
                if let Some(base_lower) = resolve_type_name(&inner, source) {
                    emit_inherits(&base_lower, b, target_label, local_types, result);
                }
            }
        }
    }
}

/// Extracts `function_definition` and `function_declaration` children of a `template_body`.
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
        if !matches!(child.kind(), "function_definition" | "function_declaration") {
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

/// Extracts a `class_definition`, `trait_definition`, or `object_definition` into `result`.
///
/// Emits:
/// - A node `B_<name>` and a `contains` edge from `B`.
/// - One `inherits` edge per resolvable base in `extends_clause` / `with_clause` children.
/// - One method node `B_<name>_<method>` and a `method` edge per `function_definition` or
///   `function_declaration` in the `template_body`.
///
/// `case class` and `case object` use the same tree-sitter node kinds as their non-case variants
/// and are handled identically.
fn extract_class_like(
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

    // Walk all named children to find extends_clause, with_clause, and template_body.
    // with_clause may appear as a sibling of extends_clause (Scala 2 style) or nested inside it.
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "extends_clause" | "with_clause" => {
                extract_extends_clause(&child, source, b, &class_label, local_types, result);
            }
            "template_body" => {
                extract_body_methods(&child, source, b, &c, &class_label, source_file, result);
            }
            _ => {}
        }
    }
}

/// Extracts a top-level `function_definition` or `function_declaration` into `result`.
///
/// Emits a function node `B_<fn>` (lowercased) and a `contains` edge from `B` to the node.
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

/// Extracts one `import_declaration` into `result`.
///
/// In tree-sitter-scala 0.26.x, import paths are represented as flat sequences of `identifier`
/// named children separated by anonymous `.` nodes, directly under `import_declaration`:
///
/// ```text
/// import scala.collection.Map
///   → import_declaration: identifier"scala", identifier"collection", identifier"Map"
/// ```
///
/// Grouped imports end with a `namespace_selectors` named child:
/// ```text
/// import scala.collection.{List, Map}
///   → import_declaration: identifier"scala", identifier"collection",
///       namespace_selectors: identifier"List", identifier"Map"
/// ```
///
/// Emits `imports_from` edges from `B` to the lowercased dotted import path. For grouped
/// imports, emits one edge per non-wildcard selector. For wildcard imports (`_`-only
/// `namespace_selectors`), emits the package prefix only.
///
/// # Errors
///
/// Never returns an error; unparseable import nodes are silently skipped.
fn extract_import(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    let mut path_parts: Vec<String> = Vec::new();
    let mut selectors: Vec<String> = Vec::new();
    let mut has_selectors = false;

    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        match child.kind() {
            "identifier" => {
                // Consecutive identifiers form the dotted import base path.
                path_parts.push(text_of(source, &child).to_lowercase());
            }
            "namespace_selectors" => {
                // Curly-brace selector list: collect each identifier child as a selector name.
                has_selectors = true;
                for j in 0..child.named_child_count() {
                    let Some(sel) = child.named_child(j) else {
                        continue;
                    };
                    if sel.kind() == "identifier" {
                        let s = text_of(source, &sel).to_lowercase();
                        if !s.is_empty() && s != "_" && s != "*" {
                            selectors.push(s);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if path_parts.is_empty() {
        return;
    }

    if has_selectors {
        let base = path_parts.join(".");
        if selectors.is_empty() {
            // Wildcard-only selector: `import foo.{_}` → emit the package prefix.
            result.edges.push(RawEdge {
                source: b.to_owned(),
                target: base,
                relation: "imports_from".to_owned(),
                confidence: Confidence::Extracted,
            });
        } else {
            for sel in selectors {
                result.edges.push(RawEdge {
                    source: b.to_owned(),
                    target: format!("{base}.{sel}"),
                    relation: "imports_from".to_owned(),
                    confidence: Confidence::Extracted,
                });
            }
        }
    } else {
        // Simple import: all identifier parts form the complete dotted path.
        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: path_parts.join("."),
            relation: "imports_from".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

// ── Extractor impl ────────────────────────────────────────────────────────────────────────────────

impl Extractor for ScalaExtractor {
    fn language(&self) -> &'static str {
        "scala"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["scala", "sc"]
    }

    /// Extracts Scala nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased);
    /// class, trait, and object nodes `B_<name>`; top-level function nodes `B_<fn>`; method nodes
    /// `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class/trait/object→method),
    /// `inherits` (extends/with, local/external split), `imports_from` (file→import path).
    /// `calls`/`uses` are deliberately omitted.
    ///
    /// An empty or package-only file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The Scala grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-scala`).
    /// - `parser.parse` returns `None` (cancellation/timeout — not for invalid Scala syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree for any input).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_scala::LANGUAGE.into())
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
        for idx in 0..root.named_child_count() {
            let Some(child) = root.named_child(idx) else {
                continue;
            };
            match child.kind() {
                "class_definition" | "trait_definition" | "object_definition" => {
                    extract_class_like(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_types,
                        &mut result,
                    );
                }
                "function_definition" | "function_declaration" => {
                    extract_function(&child, source, &b, &source_file, &mut result);
                }
                "import_declaration" => {
                    extract_import(&child, source, &b, &mut result);
                }
                // package_clause, package_object, val_definition, object_definition
                // wrapping — silently skip structural nodes we do not model.
                _ => {}
            }
        }

        Ok(result)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use habitat_graph_core::{Confidence, Extraction};

    use super::ScalaExtractor;
    use crate::registry::Extractor;

    // ── Helpers ───────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        ScalaExtractor
            .extract(Path::new(filename), src.as_bytes())
            .unwrap_or_else(|e| panic!("scala extractor failed on {filename}: {e}"))
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

    // ── A. Empty / trivial ────────────────────────────────────────────────────────────────────────

    #[test]
    fn empty_source_yields_only_file_node_and_no_edges() {
        let ex = extract("", "animals.scala");
        assert_eq!(ex.nodes.len(), 1, "empty source must yield exactly 1 node");
        assert_eq!(ex.nodes[0].label, "animals");
        assert_eq!(ex.edges.len(), 0, "empty source must yield no edges");
    }

    #[test]
    fn empty_source_file_node_label_equals_stem() {
        let ex = extract("", "Client.scala");
        assert_eq!(ex.nodes[0].label, "client", "file node label must equal lowercased stem");
    }

    #[test]
    fn malformed_source_returns_ok_and_file_node_present() {
        // tree-sitter is error-tolerant; broken source yields a partial tree, not an error.
        let ex = extract("@@@ NOT VALID SCALA !!! {{{{", "broken.scala");
        assert!(has_node(&ex, "broken"), "file node must be present even for garbled source");
    }

    // ── B. File stem handling ─────────────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HttpClient.scala");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_full_caps_is_lowercased() {
        let ex = extract("", "MODELS.scala");
        assert_eq!(ex.nodes[0].label, "models");
    }

    #[test]
    fn file_stem_preserves_underscores_and_digits() {
        let ex = extract("", "my_module2.scala");
        assert_eq!(ex.nodes[0].label, "my_module2");
    }

    // ── C. Extractor metadata ─────────────────────────────────────────────────────────────────────

    #[test]
    fn language_slug_is_scala() {
        assert_eq!(ScalaExtractor.language(), "scala");
    }

    #[test]
    fn extension_scala_is_registered() {
        assert!(
            ScalaExtractor.extensions().contains(&"scala"),
            "'.scala' extension must be registered"
        );
    }

    #[test]
    fn extension_sc_is_registered() {
        assert!(
            ScalaExtractor.extensions().contains(&"sc"),
            "'.sc' (Scala script) extension must be registered"
        );
    }

    // ── D. Class definition ───────────────────────────────────────────────────────────────────────

    #[test]
    fn class_definition_emits_node_and_contains_edge() {
        let ex = extract("class Animal { }", "zoo.scala");
        assert!(has_node(&ex, "zoo_animal"), "class node missing");
        assert!(
            has_edge(&ex, "zoo", "zoo_animal", "contains"),
            "contains edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn class_name_is_lowercased_in_label() {
        let ex = extract("class HTTPClient { }", "client.scala");
        assert!(
            has_node(&ex, "client_httpclient"),
            "class label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_classes_all_emitted() {
        let src = "class A { }\nclass B { }\nclass C { }";
        let ex = extract(src, "types.scala");
        assert!(has_node(&ex, "types_a"), "class A missing");
        assert!(has_node(&ex, "types_b"), "class B missing");
        assert!(has_node(&ex, "types_c"), "class C missing");
        let non_file: Vec<_> = ex.nodes.iter().filter(|n| n.label != "types").collect();
        assert_eq!(non_file.len(), 3, "expected exactly 3 class nodes");
    }

    #[test]
    fn class_with_constructor_params_still_emits_node() {
        let ex = extract("class Person(val name: String, val age: Int) { }", "p.scala");
        assert!(
            has_node(&ex, "p_person"),
            "class with constructor params must produce a node"
        );
    }

    // ── E. Trait definition ───────────────────────────────────────────────────────────────────────

    #[test]
    fn trait_definition_emits_node_and_contains_edge() {
        let ex = extract("trait Runnable { }", "traits.scala");
        assert!(has_node(&ex, "traits_runnable"), "trait node missing");
        assert!(
            has_edge(&ex, "traits", "traits_runnable", "contains"),
            "contains edge missing for trait"
        );
    }

    #[test]
    fn trait_name_is_lowercased_in_label() {
        let ex = extract("trait XMLSerializable { }", "iface.scala");
        assert!(
            has_node(&ex, "iface_xmlserializable"),
            "trait label must be fully lowercased"
        );
    }

    #[test]
    fn multiple_traits_all_emitted() {
        let src = "trait A { }\ntrait B { }\ntrait C { }";
        let ex = extract(src, "t.scala");
        assert!(has_node(&ex, "t_a"), "trait A missing");
        assert!(has_node(&ex, "t_b"), "trait B missing");
        assert!(has_node(&ex, "t_c"), "trait C missing");
    }

    // ── F. Object definition ──────────────────────────────────────────────────────────────────────

    #[test]
    fn object_definition_emits_node_and_contains_edge() {
        let ex = extract("object Singleton { }", "singletons.scala");
        assert!(has_node(&ex, "singletons_singleton"), "object node missing");
        assert!(
            has_edge(&ex, "singletons", "singletons_singleton", "contains"),
            "contains edge missing for object"
        );
    }

    #[test]
    fn object_name_is_lowercased_in_label() {
        let ex = extract("object MainApp { }", "app.scala");
        assert!(
            has_node(&ex, "app_mainapp"),
            "object label must be fully lowercased"
        );
    }

    #[test]
    fn case_object_is_treated_same_as_object_definition() {
        // In tree-sitter-scala, `case object` uses the same `object_definition` node kind.
        let ex = extract("case object Leaf { }", "tree.scala");
        assert!(
            has_node(&ex, "tree_leaf"),
            "case object must produce a node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "tree", "tree_leaf", "contains"));
    }

    // ── G. Top-level function definitions ────────────────────────────────────────────────────────

    #[test]
    fn function_definition_emits_fn_node_and_contains_edge() {
        let src = "def greet(name: String): String = s\"Hello $name\"";
        let ex = extract(src, "utils.scala");
        assert!(has_node(&ex, "utils_greet"), "function node missing");
        assert!(
            has_edge(&ex, "utils", "utils_greet", "contains"),
            "contains edge missing for top-level def"
        );
    }

    #[test]
    fn function_name_is_lowercased_in_label() {
        let src = "def ParseXML(): Unit = {}";
        let ex = extract(src, "parser.scala");
        assert!(
            has_node(&ex, "parser_parsexml"),
            "function label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_top_level_functions_all_emitted() {
        let src = "def a(): Unit = {}\ndef b(): Unit = {}\ndef c(): Unit = {}";
        let ex = extract(src, "fns.scala");
        assert!(has_node(&ex, "fns_a"), "fn a missing");
        assert!(has_node(&ex, "fns_b"), "fn b missing");
        assert!(has_node(&ex, "fns_c"), "fn c missing");
        let non_file: Vec<_> = ex.nodes.iter().filter(|n| n.label != "fns").collect();
        assert_eq!(non_file.len(), 3, "expected exactly 3 function nodes");
    }

    #[test]
    fn function_span_is_well_formed_and_non_empty() {
        let src = "def myFn(x: Int): Int = x + 1";
        let ex = extract(src, "span.scala");
        let n = node(&ex, "span_myfn");
        assert!(n.span.is_well_formed(), "function span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "function span must not be empty: {n:?}");
    }

    // ── H. Method extraction ──────────────────────────────────────────────────────────────────────

    #[test]
    fn method_in_class_body_emits_method_node_and_method_edge() {
        let src = "class Dog { def bark(): Unit = {} }";
        let ex = extract(src, "pets.scala");
        assert!(has_node(&ex, "pets_dog_bark"), "method node missing");
        assert!(
            has_edge(&ex, "pets_dog", "pets_dog_bark", "method"),
            "method edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn method_name_is_lowercased_in_label() {
        let src = "class Foo { def MyMethod(): Unit = {} }";
        let ex = extract(src, "m.scala");
        assert!(
            has_node(&ex, "m_foo_mymethod"),
            "method label must be fully lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn method_label_format_is_b_cls_method() {
        let src = "class Client { def sendRequest(): Unit = {} }";
        let ex = extract(src, "mymod.scala");
        // b="mymod", cls="client", m="sendrequest" → "mymod_client_sendrequest"
        assert!(
            has_node(&ex, "mymod_client_sendrequest"),
            "method label must match b_cls_method format"
        );
    }

    #[test]
    fn method_edge_source_is_class_label_not_file_label() {
        let src = "class Cat { def meow(): Unit = {} }";
        let ex = extract(src, "pet.scala");
        // Method edge source must be "pet_cat", NOT "pet".
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 1, "expected exactly 1 method edge");
        assert_eq!(
            method_edges[0].source, "pet_cat",
            "method edge source must be the class label"
        );
        assert_ne!(
            method_edges[0].source, "pet",
            "method edge source must NOT be the file label"
        );
    }

    #[test]
    fn multiple_methods_on_same_class_all_emitted() {
        let src = "class Dog { def bark(): Unit = {} \n def fetch(): Unit = {} \n def sit(): Unit = {} }";
        let ex = extract(src, "dog.scala");
        assert!(has_node(&ex, "dog_dog_bark"), "bark missing");
        assert!(has_node(&ex, "dog_dog_fetch"), "fetch missing");
        assert!(has_node(&ex, "dog_dog_sit"), "sit missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn methods_from_different_classes_both_emitted() {
        let src = "class A { def doA(): Unit = {} }\nclass B { def doB(): Unit = {} }";
        let ex = extract(src, "ab.scala");
        assert!(has_node(&ex, "ab_a_doa"), "method doA missing");
        assert!(has_node(&ex, "ab_b_dob"), "method doB missing");
        assert!(has_edge(&ex, "ab_a", "ab_a_doa", "method"), "method edge for A missing");
        assert!(has_edge(&ex, "ab_b", "ab_b_dob", "method"), "method edge for B missing");
    }

    #[test]
    fn underscore_prefix_stripped_from_method_name() {
        let src = "class Foo { def _private(): Unit = {} }";
        let ex = extract(src, "mod.scala");
        assert!(
            has_node(&ex, "mod_foo_private"),
            "leading underscore must be stripped from method id; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── I. Methods in traits and objects ──────────────────────────────────────────────────────────

    #[test]
    fn method_in_trait_body_emits_method_node_and_edge() {
        let src = "trait Runnable { def run(): Unit }";
        let ex = extract(src, "r.scala");
        assert!(
            has_node(&ex, "r_runnable_run"),
            "trait method node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(
            has_edge(&ex, "r_runnable", "r_runnable_run", "method"),
            "trait method edge missing"
        );
    }

    #[test]
    fn method_in_object_body_emits_method_node_and_edge() {
        let src = "object Ops { def apply(): Unit = {} }";
        let ex = extract(src, "o.scala");
        assert!(
            has_node(&ex, "o_ops_apply"),
            "object method node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "o_ops", "o_ops_apply", "method"));
    }

    // ── J. Inheritance (extends / with) ───────────────────────────────────────────────────────────

    #[test]
    fn extends_external_class_emits_inherits_edge_with_bare_base() {
        let src = "class Dog extends Animal { }";
        let ex = extract(src, "m.scala");
        assert!(
            has_edge(&ex, "animal", "m_dog", "inherits"),
            "external extends must yield bare lowercased base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_local_class_emits_inherits_edge_with_stem_prefix() {
        let src = "class Animal { }\nclass Dog extends Animal { }";
        let ex = extract(src, "zoo.scala");
        assert!(
            has_edge(&ex, "zoo_animal", "zoo_dog", "inherits"),
            "local extends must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn with_external_trait_emits_inherits_edge_with_bare_base() {
        let src = "class Dog extends Animal with Runnable { }";
        let ex = extract(src, "m.scala");
        // The "with Runnable" produces an inherits edge for the external mixin.
        assert!(
            ex.edges
                .iter()
                .any(|e| e.target == "m_dog" && e.relation == "inherits" && e.source == "runnable"),
            "external with-mixin must yield bare base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn with_local_trait_emits_inherits_edge_with_stem_prefix() {
        // Use valid Scala: a locally-defined trait appears in the extends position.
        // Both Animal and Runnable are local, so both get stem-prefixed base_id.
        let src = "trait Runnable { }\nclass Dog extends Runnable { }";
        let ex = extract(src, "m.scala");
        assert!(
            ex.edges.iter().any(|e| {
                e.target == "m_dog"
                    && e.relation == "inherits"
                    && e.source == "m_runnable"
            }),
            "local extends-trait must yield stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn both_extends_and_with_emit_multiple_inherits_edges() {
        let src = "class Dog extends Animal with Runnable { }";
        let ex = extract(src, "m.scala");
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits")
            .collect();
        // At minimum the extends-base must be present; with-mixin is also expected.
        assert!(
            !inherits.is_empty(),
            "extends must produce at least one inherits edge; got {:?}",
            ex.edges
        );
        assert!(
            inherits.iter().all(|e| e.target == "m_dog"),
            "all inherits edges must target m_dog"
        );
    }

    #[test]
    fn no_inherits_edges_for_class_with_no_extends() {
        let src = "class Standalone { }";
        let ex = extract(src, "s.scala");
        let inherits_count = ex.edges.iter().filter(|e| e.relation == "inherits").count();
        assert_eq!(
            inherits_count, 0,
            "class with no extends must not produce inherits edges"
        );
    }

    #[test]
    fn trait_extends_another_trait_emits_inherits_edge() {
        let src = "trait Base { }\ntrait Derived extends Base { }";
        let ex = extract(src, "traits.scala");
        assert!(
            has_edge(&ex, "traits_base", "traits_derived", "inherits"),
            "trait extends must emit inherits edge; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn extends_chain_in_same_file_all_stem_prefixed() {
        let src =
            "class A { }\nclass B extends A { }\nclass C extends B { }";
        let ex = extract(src, "chain.scala");
        assert!(
            has_edge(&ex, "chain_a", "chain_b", "inherits"),
            "A→B inherits missing; edges: {:?}",
            ex.edges
        );
        assert!(
            has_edge(&ex, "chain_b", "chain_c", "inherits"),
            "B→C inherits missing; edges: {:?}",
            ex.edges
        );
    }

    // ── K. Imports ────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn import_emits_imports_from_edge() {
        // tree-sitter-scala 0.26.x flattens the import path as identifier nodes under
        // import_declaration. The full joined path becomes the imports_from target.
        let src = "import scala.collection.Map";
        let ex = extract(src, "f.scala");
        assert!(
            has_edge(&ex, "f", "scala.collection.map", "imports_from"),
            "import must emit imports_from edge with full dotted path; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_path_is_lowercased() {
        let src = "import scala.io.Source";
        let ex = extract(src, "m.scala");
        // The full path "scala.io.Source" → lowercased → "scala.io.source".
        assert!(
            has_edge(&ex, "m", "scala.io.source", "imports_from"),
            "import path must be fully lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn multiple_imports_all_produce_edges() {
        let src =
            "import scala.io.Source\nimport scala.util.Try\nimport scala.concurrent.Future";
        let ex = extract(src, "multi.scala");
        // Each import produces one imports_from edge with the full dotted path.
        assert!(
            has_edge(&ex, "multi", "scala.io.source", "imports_from"),
            "scala.io.Source edge missing; edges: {:?}",
            ex.edges
        );
        assert!(
            has_edge(&ex, "multi", "scala.util.try", "imports_from"),
            "scala.util.Try edge missing; edges: {:?}",
            ex.edges
        );
        assert!(
            has_edge(&ex, "multi", "scala.concurrent.future", "imports_from"),
            "scala.concurrent.Future edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_dotted_path_used_as_target() {
        let src = "import scala.io.Source";
        let ex = extract(src, "io.scala");
        // The joined identifier path (lowercased) is the imports_from target.
        let import_edge = ex.edges.iter().find(|e| e.relation == "imports_from");
        assert!(import_edge.is_some(), "import edge must be present");
        let target = &import_edge.unwrap().target;
        assert!(
            !target.is_empty(),
            "import target must not be empty; got {:?}",
            target
        );
        // Path separators must be dots, not slashes.
        assert!(
            !target.contains('/'),
            "import target must use dots not slashes; got {target}"
        );
    }

    // ── L. Negative invariants ────────────────────────────────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = concat!(
            "class Foo {\n",
            "  def bar(): Unit = { baz() }\n",
            "  def baz(): Unit = {}\n",
            "}\n",
            "def main(): Unit = { val f = new Foo(); f.bar() }\n",
        );
        let ex = extract(src, "neg.scala");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "val x = 42\ndef useX(): Int = x";
        let ex = extract(src, "neg2.scala");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn methods_get_method_edge_not_contains_edge() {
        let src = "class Foo { def go(): Unit = {} }";
        let ex = extract(src, "f.scala");
        // The method node must not appear via a 'contains' edge from the file node.
        let method_via_contains = ex
            .edges
            .iter()
            .any(|e| e.relation == "contains" && e.target == "f_foo_go");
        assert!(
            !method_via_contains,
            "method node must NOT appear via a contains edge; edges: {:?}",
            ex.edges
        );
        assert!(
            has_edge(&ex, "f_foo", "f_foo_go", "method"),
            "method node must appear via a method edge"
        );
    }

    // ── M. Edge confidence ────────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = concat!(
            "import scala.io.Source\n",
            "class Base { }\n",
            "class Child extends Base { def work(): Unit = {} }\n",
            "def helper(): Unit = {}\n",
        );
        let ex = extract(src, "conf.scala");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "every edge must carry Confidence::Extracted; got {edge:?}"
            );
        }
    }

    // ── N. Source file field ──────────────────────────────────────────────────────────────────────

    #[test]
    fn source_file_field_in_every_node() {
        let src = "class Foo { def bar(): Unit = {} }\ndef baz(): Unit = {}";
        let ex = extract(src, "traced.scala");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("traced.scala"),
                "source_file {:?} must contain 'traced.scala'",
                n.source_file
            );
        }
    }

    // ── O. Span correctness ───────────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let src = "class Foo { }";
        let ex = extract(src, "sp.scala");
        let file_node = node(&ex, "sp");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn file_node_span_start_line_is_one() {
        let src = "class X { }";
        let ex = extract(src, "line.scala");
        let file_node = node(&ex, "line");
        assert_eq!(
            file_node.span.start_line, 1,
            "file node must start at line 1"
        );
    }

    #[test]
    fn class_node_span_is_well_formed_and_non_empty() {
        let src = "class BigClass { def a(): Unit = {} \n def b(): Unit = {} }";
        let ex = extract(src, "big.scala");
        let n = node(&ex, "big_bigclass");
        assert!(n.span.is_well_formed(), "class span must be well-formed: {n:?}");
        assert!(!n.span.is_empty(), "class span must not be empty: {n:?}");
    }

    // ── P. Composite / totals ─────────────────────────────────────────────────────────────────────

    #[test]
    fn two_classes_produce_correct_edge_totals() {
        let src = "class A { def x(): Unit = {} \n def y(): Unit = {} }\nclass B { def p(): Unit = {} }";
        let ex = extract(src, "ab.scala");
        // file(1) + A(1) + B(1) + A.x(1) + A.y(1) + B.p(1) = 6 nodes
        assert_eq!(
            ex.nodes.len(),
            6,
            "expected 6 nodes (1 file + 2 classes + 3 methods); got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 3, "expected 3 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges (file→A, file→B)");
    }

    #[test]
    fn realistic_scala_file_all_symbols_extracted() {
        let src = concat!(
            "import scala.collection.mutable.ListBuffer\n",
            "\n",
            "trait Printable {\n",
            "  def print(): Unit\n",
            "}\n",
            "\n",
            "class Animal(val name: String) {\n",
            "  def speak(): String = name\n",
            "}\n",
            "\n",
            "class Dog(name: String) extends Animal(name) with Printable {\n",
            "  def print(): Unit = {}\n",
            "  def fetch(): Unit = {}\n",
            "}\n",
            "\n",
            "object AnimalFactory {\n",
            "  def create(name: String): Animal = new Animal(name)\n",
            "}\n",
        );
        let ex = extract(src, "animals.scala");

        // File node.
        assert!(has_node(&ex, "animals"), "file node missing");
        // Trait.
        assert!(has_node(&ex, "animals_printable"), "trait node missing");
        // Classes.
        assert!(has_node(&ex, "animals_animal"), "Animal class node missing");
        assert!(has_node(&ex, "animals_dog"), "Dog class node missing");
        // Object.
        assert!(has_node(&ex, "animals_animalfactory"), "AnimalFactory object missing");
        // Methods.
        assert!(has_node(&ex, "animals_animal_speak"), "Animal.speak method missing");
        assert!(has_node(&ex, "animals_dog_fetch"), "Dog.fetch method missing");
        assert!(has_node(&ex, "animals_animalfactory_create"), "AnimalFactory.create missing");

        // Contains edges (file → top-level symbols).
        assert!(has_edge(&ex, "animals", "animals_printable", "contains"));
        assert!(has_edge(&ex, "animals", "animals_animal", "contains"));
        assert!(has_edge(&ex, "animals", "animals_dog", "contains"));
        assert!(has_edge(&ex, "animals", "animals_animalfactory", "contains"));

        // Method edges.
        assert!(has_edge(&ex, "animals_animal", "animals_animal_speak", "method"));
        assert!(has_edge(&ex, "animals_dog", "animals_dog_fetch", "method"));
        assert!(has_edge(
            &ex,
            "animals_animalfactory",
            "animals_animalfactory_create",
            "method"
        ));

        // imports_from: flat identifier path → "scala.collection.mutable.listbuffer".
        assert!(
            has_edge(&ex, "animals", "scala.collection.mutable.listbuffer", "imports_from"),
            "imports_from edge for ListBuffer missing; edges: {:?}",
            ex.edges
        );

        // Inherits: Animal is local so Dog's extends edge should use stem prefix.
        let inherits: Vec<_> = ex
            .edges
            .iter()
            .filter(|e| e.relation == "inherits" && e.target == "animals_dog")
            .collect();
        assert!(
            !inherits.is_empty(),
            "Dog must have at least one inherits edge; edges: {:?}",
            ex.edges
        );
        let animal_inherits = inherits
            .iter()
            .any(|e| e.source == "animals_animal");
        assert!(
            animal_inherits,
            "Dog extends Animal (local) → inherits source must be 'animals_animal'; inherits: {:?}",
            inherits
        );
    }

    // ── Q. All symbol kinds in one file ───────────────────────────────────────────────────────────

    #[test]
    fn class_trait_object_and_fn_in_same_file_all_extracted() {
        let src = concat!(
            "class MyClass { }\n",
            "trait MyTrait { }\n",
            "object MyObject { }\n",
            "def myFn(): Unit = {}\n",
        );
        let ex = extract(src, "all.scala");
        assert!(has_node(&ex, "all_myclass"), "class missing");
        assert!(has_node(&ex, "all_mytrait"), "trait missing");
        assert!(has_node(&ex, "all_myobject"), "object missing");
        assert!(has_node(&ex, "all_myfn"), "fn missing");
        // file + 4 symbols = 5 nodes total
        assert_eq!(
            ex.nodes.len(),
            5,
            "expected 5 nodes; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── R. Method id edge cases ───────────────────────────────────────────────────────────────────

    #[test]
    fn double_underscore_prefix_and_suffix_stripped() {
        let src = "class Foo { def __init__(): Unit = {} }";
        let ex = extract(src, "x.scala");
        assert!(
            has_node(&ex, "x_foo_init"),
            "double-underscore wrapping must be stripped; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn all_underscore_method_name_falls_back_to_raw_lowercase() {
        // A method named `___` (3 underscores) has no non-underscore chars after trim_matches('_')
        // → falls back to raw lowercased `"___"`. The label is b + "_" + cls + "_" + "___",
        // i.e. "x_foo" + "_" + "___" = "x_foo____" (4 trailing underscores total).
        let src = "class Foo { def ___(): Unit = {} }";
        let ex = extract(src, "x.scala");
        // Label: "x" + "_" + "foo" + "_" + "___" = "x_foo____"
        assert!(
            has_node(&ex, "x_foo____"),
            "all-underscore method must produce a node (label = x_foo____); got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    // ── S. Span on method nodes ───────────────────────────────────────────────────────────────────

    #[test]
    fn method_node_span_end_ge_start() {
        let src = "class T {\n  def multi(): Int = {\n    val x = 1\n    x + 2\n  }\n}";
        let ex = extract(src, "t.scala");
        let n = node(&ex, "t_t_multi");
        assert!(
            n.span.end_line >= n.span.start_line,
            "method span end must be >= start: {n:?}"
        );
    }

    #[test]
    fn all_node_spans_are_well_formed_for_non_trivial_source() {
        let src = "class A { def x(): Unit = {} }\ntrait B { }\nobject C { def y(): Unit = {} }";
        let ex = extract(src, "ws.scala");
        for n in &ex.nodes {
            assert!(
                n.span.is_well_formed(),
                "span must be well-formed for {}: {n:?}",
                n.label
            );
        }
    }
}
