//! JavaScript AST extractor (`tree-sitter-javascript`) — graphify qualified-id taxonomy.
//!
//! For a file with basename `B` (lowercased file stem, via [`crate::ast::util::stem_lower`]):
//! one file node `B`; class nodes `B_<cls>`; function nodes `B_<fn>`; method nodes
//! `B_<cls>_<method>`. Edges: `contains` (file→symbol), `method` (class→method),
//! `inherits` (base→derived, with local/external split), `imports_from` (file→module).
//!
//! `calls`/`uses` are deliberately NOT emitted. `CommonJS` `require()` is a `call_expression`
//! and is therefore also omitted — emitting it would require name-resolution heuristics that
//! would not match graphify's committed goldens.
//!
//! Handles `.js`, `.jsx`, `.mjs`, `.cjs` extensions. Exported declarations
//! (`export function …`, `export class …`, `export const x = () => …`) are unwrapped and
//! treated as top-level symbols identically to non-exported ones.

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::{Confidence, Extraction, GraphError, RawEdge, RawNode, Result};

use crate::ast::util::{make_span, stem_lower, text_of};
use crate::registry::Extractor;

// ── Private helpers ────────────────────────────────────────────────────────────────────────────

/// Produces the method component `m` of the qualified id `B_C_m`.
///
/// Strips all leading and trailing `_` characters then lowercases. Falls back to the fully
/// lowercased raw name when stripping leaves an empty string (e.g. a name consisting entirely
/// of underscores).
///
/// Examples: `constructor` → `"constructor"`, `_private` → `"private"`,
/// `getAll` → `"getall"`, `___` → `"___"`.
fn method_id(raw: &str) -> String {
    let trimmed = raw.trim_matches('_');
    if trimmed.is_empty() {
        raw.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Extracts the unquoted, lowercased string content from a `string` tree-sitter node.
///
/// Prefers the `string_fragment` named child (which excludes the surrounding quote characters).
/// Falls back to trimming the surrounding `'` or `"` characters from the raw node text in case
/// the `string_fragment` child is absent (e.g. an empty string literal `""`).
fn string_content(source: &[u8], string_node: &tree_sitter::Node<'_>) -> String {
    for i in 0..string_node.named_child_count() {
        if let Some(child) = string_node.named_child(i) {
            if child.kind() == "string_fragment" {
                return text_of(source, &child).to_lowercase();
            }
        }
    }
    // Fallback: strip surrounding quote characters from the full node text.
    let raw = text_of(source, string_node);
    raw.trim_matches(|c: char| c == '\'' || c == '"')
        .to_lowercase()
}

/// Returns the lowercased names of all top-level `class_declaration` nodes in `root`.
///
/// Includes classes inside `export_statement` wrappers (e.g. `export class Foo {}`).
/// This first pass is used to distinguish local bases (same file) from external ones when
/// emitting `inherits` edges.
fn local_class_names(root: tree_sitter::Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut names = HashSet::new();
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i) else {
            continue;
        };
        match child.kind() {
            "class_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    names.insert(text_of(source, &name_node).to_lowercase());
                }
            }
            "export_statement" => {
                if let Some(decl) = child.child_by_field_name("declaration") {
                    if decl.kind() == "class_declaration" {
                        if let Some(name_node) = decl.child_by_field_name("name") {
                            names.insert(text_of(source, &name_node).to_lowercase());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    names
}

/// Extracts one class declaration into `result`.
///
/// Emits:
/// - A class node `B_c`.
/// - A `contains` edge from `B` to the class node.
/// - One `inherits` edge for a simple-identifier `extends` base (`member_expression` and
///   other complex bases are skipped, matching Python's attribute-base skip policy).
/// - One method node `B_c_m` and one `method` edge per `method_definition` in the class body
///   whose name is a `property_identifier` or `private_property_identifier` (the `#` prefix
///   is stripped; `computed_property_name`, `string`, and `number` names are skipped).
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

    // Walk all named children of the class_declaration to locate `class_heritage`.
    // `class_heritage` is a named non-field child (the `extends Foo` clause).
    for i in 0..node.named_child_count() {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if child.kind() != "class_heritage" {
            continue;
        }
        // The heritage node has exactly one expression child.
        if let Some(base_node) = child.named_child(0) {
            // Only handle plain identifiers — skip `member_expression` (`mod.Base`),
            // `call_expression`, and any other complex expression base.
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

    // Methods: walk the class_body members.
    let Some(body_node) = node.child_by_field_name("body") else {
        return;
    };
    for i in 0..body_node.named_child_count() {
        let Some(member) = body_node.named_child(i) else {
            continue;
        };
        if member.kind() != "method_definition" {
            continue;
        }
        let Some(mname_node) = member.child_by_field_name("name") else {
            continue;
        };
        // Determine the raw method name based on the name node kind.
        let raw_method = match mname_node.kind() {
            "property_identifier" => text_of(source, &mname_node),
            "private_property_identifier" => {
                // Tree-sitter includes the leading '#' in the text; strip it before applying
                // method_id so that `#priv` → `"priv"` (not `"#priv"`).
                let raw = text_of(source, &mname_node);
                raw.trim_start_matches('#').to_owned()
            }
            // computed_property_name ([Symbol.iterator]), string keys, numeric keys → skip.
            _ => continue,
        };
        let m = method_id(&raw_method);
        let method_label = format!("{b}_{c}_{m}");

        result.nodes.push(RawNode {
            label: method_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&member),
        });

        // method: class → method (NOT contains)
        result.edges.push(RawEdge {
            source: class_label.clone(),
            target: method_label,
            relation: "method".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

/// Extracts one top-level `function_declaration` or `generator_function_declaration` into `result`.
///
/// Emits a function node `B_f` and a `contains` edge from `B` to the node. The `name` field
/// is required; declarations without one (anonymous `export default function() {}`) are silently
/// skipped.
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

/// Extracts `arrow_function` and `function_expression` values from a `lexical_declaration` or
/// `variable_declaration` node.
///
/// Each `variable_declarator` whose `value` is an `arrow_function` or `function_expression` and
/// whose `name` is a plain `identifier` (not a destructuring pattern) emits a function node
/// `B_f` and a `contains` edge.  Destructured names (`const { a } = …`), non-function values,
/// and `generator_function` values are silently skipped.
fn extract_var_decl(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    result: &mut Extraction,
) {
    for i in 0..node.named_child_count() {
        let Some(declarator) = node.named_child(i) else {
            continue;
        };
        if declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = declarator.child_by_field_name("name") else {
            continue;
        };
        // Only plain identifier LHS; skip array/object destructuring.
        if name_node.kind() != "identifier" {
            continue;
        }
        let Some(value_node) = declarator.child_by_field_name("value") else {
            continue;
        };
        // Only arrow functions and function expressions produce a B_fn node.
        if value_node.kind() != "arrow_function" && value_node.kind() != "function_expression" {
            continue;
        }
        let fn_lower = text_of(source, &name_node).to_lowercase();
        let fn_label = format!("{b}_{fn_lower}");

        result.nodes.push(RawNode {
            label: fn_label.clone(),
            source_file: source_file.to_owned(),
            span: make_span(&declarator),
        });

        result.edges.push(RawEdge {
            source: b.to_owned(),
            target: fn_label,
            relation: "contains".to_owned(),
            confidence: Confidence::Extracted,
        });
    }
}

/// Extracts an `import_statement` into `result`.
///
/// Emits an `imports_from` edge from `B` to the lowercased module path from the `source` field.
/// Side-effect-only imports (`import 'polyfill'`) are included — the source string is always
/// present. Dynamic `import()` calls are `call_expression` nodes and are therefore silently
/// omitted.
fn extract_import(node: &tree_sitter::Node<'_>, source: &[u8], b: &str, result: &mut Extraction) {
    let Some(source_node) = node.child_by_field_name("source") else {
        return;
    };
    let module = string_content(source, &source_node);
    result.edges.push(RawEdge {
        source: b.to_owned(),
        target: module,
        relation: "imports_from".to_owned(),
        confidence: Confidence::Extracted,
    });
}

/// Unwraps an `export_statement` and processes its `declaration` field as a top-level item.
///
/// Handles `export function`, `export class`, `export const/let/var`, and
/// `export function*`. `export default expression` (which uses the `value` field, not
/// `declaration`) is silently skipped. `export { … } from 'module'` re-exports are also
/// skipped — only `import_statement` nodes produce `imports_from` edges.
fn extract_export(
    node: &tree_sitter::Node<'_>,
    source: &[u8],
    b: &str,
    source_file: &str,
    local_classes: &HashSet<String>,
    result: &mut Extraction,
) {
    let Some(decl) = node.child_by_field_name("declaration") else {
        return;
    };
    match decl.kind() {
        "function_declaration" | "generator_function_declaration" => {
            extract_toplevel_fn(&decl, source, b, source_file, result);
        }
        "class_declaration" => {
            extract_class(&decl, source, b, source_file, local_classes, result);
        }
        "lexical_declaration" | "variable_declaration" => {
            extract_var_decl(&decl, source, b, source_file, result);
        }
        _ => {}
    }
}

// ── Extractor impl ─────────────────────────────────────────────────────────────────────────────

/// Extracts nodes/edges from JavaScript / JSX source using `tree-sitter-javascript`, in
/// **graphify's qualified-id taxonomy** (so output is comparable to graphify's committed goldens).
///
/// For a file with basename `B` (filename without extension, lowercased): one file node `B`;
/// class/function nodes `B_<name>`; method nodes `B_<class>_<method>`. Edges: `contains`
/// (file→symbol), `method` (class→method), `inherits` (base→derived, with external base nodes),
/// `imports_from` (file→module). `calls`/`uses` are deliberately NOT emitted. `CommonJS`
/// `require()` is a `call_expression` and is also omitted.
#[derive(Debug, Default, Clone, Copy)]
pub struct JsExtractor;

impl Extractor for JsExtractor {
    fn language(&self) -> &'static str {
        "javascript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "jsx", "mjs", "cjs"]
    }

    /// Extracts JavaScript nodes and edges from the bytes at `path`.
    ///
    /// Produces graphify's qualified-id taxonomy: a file node `B` (file stem, lowercased); class
    /// nodes `B_<cls>`; function nodes `B_<fn>`; method nodes `B_<cls>_<method>`; with `contains`,
    /// `method`, `inherits`, and `imports_from` edges. `calls`/`uses` are deliberately omitted.
    ///
    /// An empty file produces exactly one node (the file node) and no edges.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::Parse`] if:
    /// - The JavaScript grammar could not be installed on the parser (should never occur with a
    ///   correctly linked `tree-sitter-javascript`).
    /// - `parser.parse` returns `None` (cancellation / timeout — not for invalid JS syntax;
    ///   tree-sitter is error-tolerant and always produces a partial tree).
    fn extract(&self, path: &Path, source: &[u8]) -> Result<Extraction> {
        let source_file = path.to_string_lossy().into_owned();

        // B = lowercased file stem, e.g. "client" for "Client.js".
        let b = stem_lower(path);

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
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

        // Always emit the file node, even for empty/garbled source.
        result.nodes.push(RawNode {
            label: b.clone(),
            source_file: source_file.clone(),
            span: make_span(&root),
        });

        // Pass 2: walk top-level program children, emitting nodes/edges per kind.
        for i in 0..root.named_child_count() {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            match child.kind() {
                "function_declaration" | "generator_function_declaration" => {
                    extract_toplevel_fn(&child, source, &b, &source_file, &mut result);
                }
                "class_declaration" => {
                    extract_class(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_classes,
                        &mut result,
                    );
                }
                "lexical_declaration" | "variable_declaration" => {
                    extract_var_decl(&child, source, &b, &source_file, &mut result);
                }
                "import_statement" => {
                    extract_import(&child, source, &b, &mut result);
                }
                "export_statement" => {
                    extract_export(
                        &child,
                        source,
                        &b,
                        &source_file,
                        &local_classes,
                        &mut result,
                    );
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

    use super::JsExtractor;
    use crate::registry::Extractor;

    // ── Helpers ────────────────────────────────────────────────────────────────────────────────

    /// Run the extractor on `src` as if it came from `filename`; panic on error.
    fn extract(src: &str, filename: &str) -> Extraction {
        JsExtractor
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
        let ex = extract("", "empty.js");
        assert_eq!(ex.nodes.len(), 1, "empty source: expected exactly 1 node");
        assert_eq!(ex.nodes[0].label, "empty");
        assert_eq!(ex.edges.len(), 0, "empty source: expected no edges");
    }

    // ── 2. File stem lowercasing ───────────────────────────────────────────────────────────────

    #[test]
    fn file_stem_is_lowercased() {
        let ex = extract("", "HttpClient.js");
        assert_eq!(ex.nodes[0].label, "httpclient");
    }

    #[test]
    fn file_stem_mixed_case_fully_lowercased() {
        let ex = extract("", "MyModule.js");
        assert_eq!(ex.nodes[0].label, "mymodule");
    }

    // ── 3. Extension variants ──────────────────────────────────────────────────────────────────

    #[test]
    fn extensions_cover_js_jsx_mjs_cjs() {
        let exts = JsExtractor.extensions();
        for e in ["js", "jsx", "mjs", "cjs"] {
            assert!(exts.contains(&e), "missing extension {e}");
        }
    }

    #[test]
    fn language_slug_is_javascript() {
        assert_eq!(JsExtractor.language(), "javascript");
    }

    #[test]
    fn mjs_extension_parses_and_yields_file_node() {
        let ex = extract("export const x = 1;\n", "module.mjs");
        assert!(
            has_node(&ex, "module"),
            "file node must be present for .mjs"
        );
    }

    #[test]
    fn cjs_extension_parses_and_yields_file_node() {
        let ex = extract("const x = require('fs');\n", "loader.cjs");
        assert!(
            has_node(&ex, "loader"),
            "file node must be present for .cjs"
        );
    }

    #[test]
    fn jsx_extension_parses_and_yields_file_node() {
        let ex = extract("const el = <div/>;\n", "view.jsx");
        assert_eq!(ex.nodes[0].label, "view");
    }

    // ── 4. Top-level function declarations ────────────────────────────────────────────────────

    #[test]
    fn function_declaration_emits_fn_node_and_contains_edge() {
        let ex = extract("function hello() {}\n", "greet.js");
        assert!(has_node(&ex, "greet_hello"), "function node missing");
        assert!(
            has_edge(&ex, "greet", "greet_hello", "contains"),
            "contains edge missing"
        );
    }

    #[test]
    fn function_name_is_lowercased() {
        let ex = extract("function ParseURL(s) {}\n", "utils.js");
        assert!(
            has_node(&ex, "utils_parseurl"),
            "function label must be lowercased"
        );
    }

    #[test]
    fn generator_function_declaration_emits_fn_node() {
        let ex = extract("function* generate() { yield 1; }\n", "gen.js");
        assert!(
            has_node(&ex, "gen_generate"),
            "generator function node missing"
        );
        assert!(has_edge(&ex, "gen", "gen_generate", "contains"));
    }

    #[test]
    fn multiple_toplevel_functions_all_emitted() {
        let src = "function a() {}\nfunction b() {}\nfunction c() {}\n";
        let ex = extract(src, "funcs.js");
        assert!(has_node(&ex, "funcs_a"));
        assert!(has_node(&ex, "funcs_b"));
        assert!(has_node(&ex, "funcs_c"));
        let fn_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label != "funcs").collect();
        assert_eq!(fn_nodes.len(), 3, "expected exactly 3 function nodes");
    }

    // ── 5. Arrow functions / function expressions (lexical_declaration / variable_declaration) ─

    #[test]
    fn const_arrow_emits_fn_node_and_contains_edge() {
        let ex = extract("const greet = (name) => name;\n", "utils.js");
        assert!(has_node(&ex, "utils_greet"), "arrow function node missing");
        assert!(has_edge(&ex, "utils", "utils_greet", "contains"));
    }

    #[test]
    fn const_function_expression_emits_fn_node() {
        let ex = extract("const make = function() {};\n", "factory.js");
        assert!(
            has_node(&ex, "factory_make"),
            "function expression node missing"
        );
        assert!(has_edge(&ex, "factory", "factory_make", "contains"));
    }

    #[test]
    fn var_arrow_function_emits_fn_node() {
        let ex = extract("var cb = () => {};\n", "handlers.js");
        assert!(has_node(&ex, "handlers_cb"), "var arrow node missing");
    }

    #[test]
    fn let_arrow_function_emits_fn_node() {
        let ex = extract("let transform = (x) => x * 2;\n", "math.js");
        assert!(has_node(&ex, "math_transform"), "let arrow node missing");
    }

    #[test]
    fn non_function_const_value_is_not_emitted_as_fn() {
        let ex = extract("const MAX = 42;\n", "consts.js");
        // Only the file node; no function node for numeric literal
        assert_eq!(
            ex.nodes.len(),
            1,
            "non-function const must not produce a function node; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn destructured_const_is_not_emitted_as_fn() {
        let ex = extract("const { a, b } = obj;\n", "destruct.js");
        assert_eq!(
            ex.nodes.len(),
            1,
            "destructured const must not produce a function node"
        );
    }

    #[test]
    fn multiple_arrow_functions_in_one_const_block_emitted() {
        // Two separate const statements each with an arrow function
        let src = "const f = () => {};\nconst g = () => {};\n";
        let ex = extract(src, "multi.js");
        assert!(has_node(&ex, "multi_f"), "f missing");
        assert!(has_node(&ex, "multi_g"), "g missing");
    }

    // ── 6. Class declarations ─────────────────────────────────────────────────────────────────

    #[test]
    fn class_no_heritage_emits_class_node_and_contains_edge() {
        let ex = extract("class Dog {}\n", "animals.js");
        assert!(has_node(&ex, "animals_dog"), "class node missing");
        assert!(has_edge(&ex, "animals", "animals_dog", "contains"));
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "inherits").count(),
            0,
            "no heritage → no inherits edges"
        );
    }

    #[test]
    fn class_name_is_lowercased() {
        let ex = extract("class HTTPClient {}\n", "client.js");
        assert!(
            has_node(&ex, "client_httpclient"),
            "class label must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_classes_all_emitted() {
        let src = "class A {}\nclass B {}\nclass C {}\n";
        let ex = extract(src, "classes.js");
        assert!(has_node(&ex, "classes_a"));
        assert!(has_node(&ex, "classes_b"));
        assert!(has_node(&ex, "classes_c"));
        assert_eq!(
            ex.edges.iter().filter(|e| e.relation == "contains").count(),
            3,
            "expected 3 contains edges"
        );
    }

    // ── 7. Class heritage / inherits ──────────────────────────────────────────────────────────

    #[test]
    fn external_base_yields_bare_lowercased_base_id() {
        let ex = extract("class Poodle extends Dog {}\n", "breeds.js");
        assert!(has_node(&ex, "breeds_poodle"));
        assert!(
            has_edge(&ex, "dog", "breeds_poodle", "inherits"),
            "inherits edge with external base missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn local_base_uses_stem_prefixed_base_id() {
        let src = "class A {}\nclass B extends A {}\n";
        let ex = extract(src, "m.js");
        assert!(
            has_edge(&ex, "m_a", "m_b", "inherits"),
            "local base must produce stem-prefixed base_id; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn member_expression_base_produces_no_inherits_edge() {
        let ex = extract("class Foo extends mod.Bar {}\n", "a.js");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "inherits"),
            "member_expression base must NOT produce an inherits edge; got {:?}",
            ex.edges
        );
    }

    #[test]
    fn mixed_local_and_external_bases_classified_correctly() {
        // In JS a class can only extend ONE base, but let's test the two-class local/external split.
        let src = "class Base {}\nclass Child extends Base {}\n";
        let ex = extract(src, "mix.js");
        assert!(
            has_edge(&ex, "mix_base", "mix_child", "inherits"),
            "local base must use stem-prefixed id"
        );
    }

    #[test]
    fn class_extends_uses_correct_base_id_for_external() {
        let src = "class Widget extends EventEmitter {}\n";
        let ex = extract(src, "widgets.js");
        // EventEmitter is not defined in this file → external → bare "eventemitter"
        assert!(
            has_edge(&ex, "eventemitter", "widgets_widget", "inherits"),
            "external base should be bare lowercased; edges: {:?}",
            ex.edges
        );
    }

    // ── 8. Method definitions ─────────────────────────────────────────────────────────────────

    #[test]
    fn method_emits_node_and_method_edge() {
        let src = "class Dog { bark() {} }\n";
        let ex = extract(src, "animals.js");
        assert!(
            has_node(&ex, "animals_dog_bark"),
            "method node missing; nodes: {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
        assert!(has_edge(&ex, "animals_dog", "animals_dog_bark", "method"));
    }

    #[test]
    fn constructor_method_emits_node() {
        let src = "class Person { constructor(name) {} }\n";
        let ex = extract(src, "model.js");
        assert!(
            has_node(&ex, "model_person_constructor"),
            "constructor method node missing; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn static_method_emits_node() {
        let src = "class Factory { static create() {} }\n";
        let ex = extract(src, "factory.js");
        assert!(
            has_node(&ex, "factory_factory_create"),
            "static method node missing; nodes: {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn private_method_strips_hash_and_emits_node() {
        let src = "class A { #priv() {} }\n";
        let ex = extract(src, "mod.js");
        assert!(
            has_node(&ex, "mod_a_priv"),
            "private method node must strip '#'; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn computed_method_is_skipped() {
        let src = "class A { [Symbol.iterator]() {} }\n";
        let ex = extract(src, "itr.js");
        // Only the file node and class node, no method node
        let method_nodes: Vec<_> = ex
            .nodes
            .iter()
            .filter(|n| {
                ex.edges
                    .iter()
                    .any(|e| e.target == n.label && e.relation == "method")
            })
            .collect();
        assert!(
            method_nodes.is_empty(),
            "computed method must be skipped; got {method_nodes:?}"
        );
    }

    #[test]
    fn method_name_is_lowercased() {
        let src = "class Foo { myMethod() {} }\n";
        let ex = extract(src, "mod.js");
        assert!(
            has_node(&ex, "mod_foo_mymethod"),
            "method label must be lowercased; got {:?}",
            ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn multiple_methods_all_emitted_with_method_edges() {
        let src = "class Foo { a() {} b() {} c() {} }\n";
        let ex = extract(src, "foo.js");
        assert!(has_node(&ex, "foo_foo_a"), "method a missing");
        assert!(has_node(&ex, "foo_foo_b"), "method b missing");
        assert!(has_node(&ex, "foo_foo_c"), "method c missing");
        let method_edges: Vec<_> = ex.edges.iter().filter(|e| e.relation == "method").collect();
        assert_eq!(method_edges.len(), 3, "expected 3 method edges");
    }

    #[test]
    fn method_edge_is_method_not_contains() {
        let src = "class A { m() {} }\n";
        let ex = extract(src, "a.js");
        // method nodes must use "method" relation, not "contains"
        assert!(
            has_edge(&ex, "a_a", "a_a_m", "method"),
            "method edge missing"
        );
        assert!(
            !has_edge(&ex, "a_a", "a_a_m", "contains"),
            "method node must NOT also have a contains edge"
        );
    }

    // ── 9. Import statements ──────────────────────────────────────────────────────────────────

    #[test]
    fn import_statement_emits_imports_from_edge() {
        let ex = extract("import { readFile } from 'fs';\n", "io.js");
        assert!(
            has_edge(&ex, "io", "fs", "imports_from"),
            "imports_from edge missing; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_default_emits_imports_from_edge() {
        let ex = extract("import fs from 'fs';\n", "io.js");
        assert!(has_edge(&ex, "io", "fs", "imports_from"));
    }

    #[test]
    fn import_path_is_lowercased() {
        let ex = extract("import x from './HTTP/Client';\n", "mod.js");
        assert!(
            has_edge(&ex, "mod", "./http/client", "imports_from"),
            "import path must be lowercased; edges: {:?}",
            ex.edges
        );
    }

    #[test]
    fn import_double_quote_source_works() {
        let ex = extract("import { a } from \"lodash\";\n", "lib.js");
        assert!(has_edge(&ex, "lib", "lodash", "imports_from"));
    }

    #[test]
    fn multiple_imports_all_emitted() {
        let src = "import a from 'axios';\nimport b from 'lodash';\n";
        let ex = extract(src, "deps.js");
        assert!(
            has_edge(&ex, "deps", "axios", "imports_from"),
            "axios edge missing"
        );
        assert!(
            has_edge(&ex, "deps", "lodash", "imports_from"),
            "lodash edge missing"
        );
    }

    // ── 10. Export statement ──────────────────────────────────────────────────────────────────

    #[test]
    fn exported_function_declaration_emits_fn_node() {
        let ex = extract("export function exported() {}\n", "api.js");
        assert!(
            has_node(&ex, "api_exported"),
            "exported function node missing"
        );
        assert!(has_edge(&ex, "api", "api_exported", "contains"));
    }

    #[test]
    fn exported_class_declaration_emits_class_node() {
        let ex = extract("export class Widget {}\n", "ui.js");
        assert!(has_node(&ex, "ui_widget"), "exported class node missing");
        assert!(has_edge(&ex, "ui", "ui_widget", "contains"));
    }

    #[test]
    fn exported_const_arrow_emits_fn_node() {
        let ex = extract("export const handler = () => {};\n", "routes.js");
        assert!(
            has_node(&ex, "routes_handler"),
            "exported arrow node missing"
        );
        assert!(has_edge(&ex, "routes", "routes_handler", "contains"));
    }

    #[test]
    fn exported_generator_fn_emits_fn_node() {
        let ex = extract("export function* ids() { yield 1; }\n", "seq.js");
        assert!(has_node(&ex, "seq_ids"), "exported generator node missing");
    }

    #[test]
    fn exported_class_with_local_base_classified_correctly() {
        let src = "class Base {}\nexport class Derived extends Base {}\n";
        let ex = extract(src, "hier.js");
        // Base is local → "hier_base"
        assert!(
            has_edge(&ex, "hier_base", "hier_derived", "inherits"),
            "exported class: local base must use stem-prefixed id; edges: {:?}",
            ex.edges
        );
    }

    // ── 11. Negative invariant: no calls or uses edges ────────────────────────────────────────

    #[test]
    fn no_calls_edges_ever_emitted() {
        let src = "class Foo { bar() { baz(); } }\nfunction baz() {}\nconst x = baz();\n";
        let ex = extract(src, "noisy.js");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "calls",
                "calls edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn no_uses_edges_ever_emitted() {
        let src = "const X = 1;\nfunction f() { return X; }\n";
        let ex = extract(src, "noisy.js");
        for edge in &ex.edges {
            assert_ne!(
                edge.relation, "uses",
                "uses edge must never be emitted; got {edge:?}"
            );
        }
    }

    #[test]
    fn commonjs_require_does_not_emit_imports_from_edge() {
        // require() is a call_expression — deliberately NOT emitted.
        let ex = extract("const fs = require('fs');\n", "cjs.js");
        assert!(
            !ex.edges.iter().any(|e| e.relation == "imports_from"),
            "CommonJS require must NOT produce an imports_from edge; got {:?}",
            ex.edges
        );
    }

    // ── 12. Confidence and metadata ───────────────────────────────────────────────────────────

    #[test]
    fn all_edges_carry_extracted_confidence() {
        let src = "class Foo extends Bar { m() {} }\nimport x from 'x';\n";
        let ex = extract(src, "f.js");
        for edge in &ex.edges {
            assert_eq!(
                edge.confidence,
                Confidence::Extracted,
                "edge {edge:?} must be Extracted"
            );
        }
    }

    #[test]
    fn source_file_field_in_every_node() {
        let src = "class Foo { m() {} }\nfunction bar() {}\n";
        let ex = extract(src, "mymod.js");
        for n in &ex.nodes {
            assert!(
                n.source_file.contains("mymod.js"),
                "source_file {:?} must contain 'mymod.js'",
                n.source_file
            );
        }
    }

    // ── 13. Span correctness ──────────────────────────────────────────────────────────────────

    #[test]
    fn file_node_span_starts_at_byte_zero() {
        let src = "class Foo {}\n";
        let ex = extract(src, "span.js");
        let file_node = node(&ex, "span");
        assert_eq!(
            file_node.span.start_byte, 0,
            "file node span must start at byte 0"
        );
    }

    #[test]
    fn class_on_first_line_has_start_line_one() {
        let src = "class Foo {}\n";
        let ex = extract(src, "x.js");
        let n = node(&ex, "x_foo");
        assert_eq!(
            n.span.start_line, 1,
            "class on line 1 must have start_line=1"
        );
    }

    #[test]
    fn class_on_third_line_has_correct_start_line() {
        let src = "\n\nclass Foo {}\n";
        let ex = extract(src, "y.js");
        let n = node(&ex, "y_foo");
        assert_eq!(
            n.span.start_line, 3,
            "class on line 3 must have start_line=3; got {n:?}"
        );
    }

    #[test]
    fn class_node_span_is_well_formed_and_non_empty() {
        let src = "class Foo { bar() {} }\n";
        let ex = extract(src, "x.js");
        let n = node(&ex, "x_foo");
        assert!(
            n.span.is_well_formed(),
            "class span must be well-formed: {n:?}"
        );
        assert!(!n.span.is_empty(), "class span must not be empty: {n:?}");
    }

    #[test]
    fn method_span_has_valid_start_end_lines() {
        let src = "class Foo {\n  multi() {\n    return 1;\n  }\n}\n";
        let ex = extract(src, "sl.js");
        let n = node(&ex, "sl_foo_multi");
        assert!(
            n.span.end_line >= n.span.start_line,
            "end_line must be >= start_line: {n:?}"
        );
    }

    // ── 14. Nesting invariants ─────────────────────────────────────────────────────────────────

    #[test]
    fn nested_class_not_emitted_as_top_level_class_node() {
        // Inner class inside a method body must NOT appear as a top-level class.
        let src = "class Outer { m() { class Inner {} } }\n";
        let ex = extract(src, "nested.js");
        assert!(has_node(&ex, "nested_outer"), "outer class must be present");
        assert!(
            !has_node(&ex, "nested_inner"),
            "nested class must NOT be emitted as a top-level node"
        );
    }

    #[test]
    fn nested_function_inside_function_not_emitted() {
        let src = "function outer() { function inner() {} }\n";
        let ex = extract(src, "funcs.js");
        assert!(
            has_node(&ex, "funcs_outer"),
            "outer function must be present"
        );
        assert!(
            !has_node(&ex, "funcs_inner"),
            "nested function must NOT be emitted"
        );
    }

    // ── 15. Mixed content ─────────────────────────────────────────────────────────────────────

    #[test]
    fn mixed_class_function_import_all_emitted() {
        let src =
            "import { a } from 'mod';\nclass Foo {}\nfunction bar() {}\nconst baz = () => {};\n";
        let ex = extract(src, "mix.js");
        assert!(has_node(&ex, "mix"), "file node missing");
        assert!(has_node(&ex, "mix_foo"), "class node missing");
        assert!(has_node(&ex, "mix_bar"), "function node missing");
        assert!(has_node(&ex, "mix_baz"), "arrow node missing");
        assert!(has_edge(&ex, "mix", "mix_foo", "contains"));
        assert!(has_edge(&ex, "mix", "mix_bar", "contains"));
        assert!(has_edge(&ex, "mix", "mix_baz", "contains"));
        assert!(has_edge(&ex, "mix", "mod", "imports_from"));
    }

    // ── 16. Total counts ──────────────────────────────────────────────────────────────────────

    #[test]
    fn two_classes_two_methods_each_produce_correct_totals() {
        let src = concat!("class A { x() {} y() {} }\n", "class B { p() {} q() {} }\n",);
        let ex = extract(src, "ab.js");
        // file + 2 classes + 4 methods = 7 nodes
        assert_eq!(
            ex.nodes.len(),
            7,
            "expected 7 nodes (1 file + 2 class + 4 method)"
        );
        let method_edges = ex.edges.iter().filter(|e| e.relation == "method").count();
        let contains_edges = ex.edges.iter().filter(|e| e.relation == "contains").count();
        assert_eq!(method_edges, 4, "expected 4 method edges");
        assert_eq!(contains_edges, 2, "expected 2 contains edges");
    }

    #[test]
    fn class_with_only_constructor_has_exactly_one_method_node() {
        let src = "class Widget { constructor() {} }\n";
        let ex = extract(src, "widgets.js");
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
        assert_eq!(method_nodes[0].label, "widgets_widget_constructor");
    }

    // ── 17. Malformed source is not an error ──────────────────────────────────────────────────

    #[test]
    fn malformed_source_returns_ok_with_file_node() {
        // tree-sitter is error-tolerant; partial parse still yields a file node.
        let ex = extract("function ( { class +++!!!\n", "broken.js");
        assert!(
            has_node(&ex, "broken"),
            "file node must be present even for garbled source"
        );
    }

    // ── 18. Spec example ──────────────────────────────────────────────────────────────────────

    #[test]
    fn spec_example_class_with_method_and_import() {
        let src =
            "import EventEmitter from 'events';\nclass MyClass extends EventEmitter { run() {} }\n";
        let ex = extract(src, "app.js");
        assert!(has_node(&ex, "app"), "file node missing");
        assert!(has_node(&ex, "app_myclass"), "class node missing");
        assert!(has_node(&ex, "app_myclass_run"), "method node missing");
        assert!(has_edge(&ex, "app", "app_myclass", "contains"));
        assert!(has_edge(&ex, "eventemitter", "app_myclass", "inherits"));
        assert!(has_edge(&ex, "app_myclass", "app_myclass_run", "method"));
        assert!(has_edge(&ex, "app", "events", "imports_from"));
    }
}
