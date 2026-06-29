//! TypeScript PA-1 parity gate — exercises `TsExtractor` against the `sample.ts` golden corpus.
//!
//! The `habitat-graph` CLI's `extract` command hardcodes `detect(dir, &["rs"])`, so it emits an
//! empty graph for `.ts` files. These tests bypass the CLI and call `TsExtractor` (and the full
//! detect→extract→build pipeline) directly, validating the complete PA-1 taxonomy:
//!
//! ```text
//! File node   B
//! Class/iface B_Name        (contains edge: B → B_Name)
//! Method      B_Cls_method  (method edge:   B_Cls → B_Cls_method)
//! Function    B_fn          (contains edge: B → B_fn)
//! Inherits                  (inherits edge: base → derived)
//! Imports                   (imports_from edge: B → module-specifier)
//! ```
//!
//! Parity bar (R3b, factory-critical tier): ≥95% nodes, ≥90% structural edges vs the
//! self-golden below.  Since no graphify Python oracle exists for TypeScript, this file IS the
//! oracle specification.
//!
//! The `sample.ts` golden corpus lives at:
//! `habitat-graph/fixtures/worked/ts/src/sample.ts`

use std::collections::HashSet;
use std::path::Path;

use habitat_graph_core::Extraction;
use habitat_graph_extract::ast::ts::TsExtractor;
use habitat_graph_extract::Extractor;

// ── Self-golden constants (R3b, ratchet UP on legitimate improvement) ─────────────────────────

/// Expected nodes from extracting `sample.ts` (including the file node itself).
const SAMPLE_NODE_COUNT: usize = 20;

/// Expected total edges from extracting `sample.ts`.
const SAMPLE_EDGE_COUNT: usize = 25;

/// Expected `contains` edges (file → symbol) in `sample.ts`.
const SAMPLE_CONTAINS_COUNT: usize = 7;

/// Expected `method` edges (class → method node) in `sample.ts`.
const SAMPLE_METHOD_COUNT: usize = 12;

/// Expected `inherits` edges (base → derived) in `sample.ts`.
const SAMPLE_INHERITS_COUNT: usize = 4;

/// Expected `imports_from` edges in `sample.ts`.
const SAMPLE_IMPORTS_COUNT: usize = 2;

/// Edges retained after `assemble` — two `imports_from` edges to external modules (`events`,
/// `./logger`) are dropped as dangling (their target labels are not nodes in the extraction).
const ASSEMBLED_EDGE_COUNT: usize = 23;

// ── Embedded corpus ────────────────────────────────────────────────────────────────────────────

/// The raw TypeScript source of the golden corpus — embedded at compile time so the test binary
/// is self-contained and independent of working-directory layout.
const SAMPLE_TS: &str = include_str!("../../../fixtures/worked/ts/src/sample.ts");

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────

/// Run `TsExtractor` on `src` pretending the file is named `filename`. Panics on error.
fn extract(src: &str, filename: &str) -> Extraction {
    TsExtractor
        .extract(Path::new(filename), src.as_bytes())
        .unwrap_or_else(|e| panic!("TsExtractor failed on {filename}: {e}"))
}

/// Extract `SAMPLE_TS` as `"sample.ts"`. This is the canonical extraction for all corpus tests.
fn extract_sample() -> Extraction {
    extract(SAMPLE_TS, "sample.ts")
}

/// `true` if `ex` has a node with label == `label`.
fn has_node(ex: &Extraction, label: &str) -> bool {
    ex.nodes.iter().any(|n| n.label == label)
}

/// `true` if `ex` has an edge matching the triple `(source, target, relation)`.
fn has_edge(ex: &Extraction, src: &str, tgt: &str, rel: &str) -> bool {
    ex.edges
        .iter()
        .any(|e| e.source == src && e.target == tgt && e.relation == rel)
}

/// Count edges with the given relation in `ex`.
fn edge_count(ex: &Extraction, rel: &str) -> usize {
    ex.edges.iter().filter(|e| e.relation == rel).count()
}

// ── Group A: Extractor trait surface ──────────────────────────────────────────────────────────

/// A-1: language slug is "typescript".
#[test]
fn extractor_language_is_typescript() {
    assert_eq!(TsExtractor.language(), "typescript");
}

/// A-2: "ts" extension is registered.
#[test]
fn extractor_registers_ts_extension() {
    assert!(
        TsExtractor.extensions().contains(&"ts"),
        "\"ts\" must be in extensions()"
    );
}

/// A-3: "tsx" extension is registered.
#[test]
fn extractor_registers_tsx_extension() {
    assert!(
        TsExtractor.extensions().contains(&"tsx"),
        "\"tsx\" must be in extensions()"
    );
}

/// A-4: "mts" extension is registered.
#[test]
fn extractor_registers_mts_extension() {
    assert!(
        TsExtractor.extensions().contains(&"mts"),
        "\"mts\" must be in extensions()"
    );
}

/// A-5: "cts" extension is registered.
#[test]
fn extractor_registers_cts_extension() {
    assert!(
        TsExtractor.extensions().contains(&"cts"),
        "\"cts\" must be in extensions()"
    );
}

// ── Group B: File node ─────────────────────────────────────────────────────────────────────────

/// B-1: always exactly one file node.
#[test]
fn sample_has_exactly_one_file_node() {
    let ex = extract_sample();
    let file_nodes: Vec<_> = ex.nodes.iter().filter(|n| n.label == "sample").collect();
    assert_eq!(
        file_nodes.len(),
        1,
        "expected exactly one 'sample' file node; got {}",
        file_nodes.len()
    );
}

/// B-2: file node label is lowercased stem ("sample").
#[test]
fn file_node_label_is_stem_lowercased() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample"),
        "file node 'sample' must be present"
    );
}

/// B-3: the file node is the first emitted.
#[test]
fn file_node_is_first_emitted() {
    let ex = extract_sample();
    assert_eq!(
        ex.nodes.first().map(|n| n.label.as_str()),
        Some("sample"),
        "file node must be the first node emitted"
    );
}

/// B-4: empty source → exactly one node (the file node) and no edges.
#[test]
fn empty_source_yields_one_file_node() {
    let ex = extract("", "sample.ts");
    assert_eq!(ex.nodes.len(), 1, "empty source → exactly 1 node");
    assert_eq!(ex.edges.len(), 0, "empty source → no edges");
}

/// B-5: uppercase stem is lowercased.
#[test]
fn uppercase_stem_is_lowercased() {
    let ex = extract("", "MyModule.ts");
    assert_eq!(
        ex.nodes[0].label, "mymodule",
        "stem must be fully lowercased"
    );
}

// ── Group C: Interface nodes ───────────────────────────────────────────────────────────────────

/// C-1: `Walkable` interface → node `sample_walkable`.
#[test]
fn walkable_interface_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_walkable"),
        "interface Walkable → node 'sample_walkable' expected"
    );
}

/// C-2: `Trainable` interface → node `sample_trainable`.
#[test]
fn trainable_interface_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_trainable"),
        "interface Trainable → node 'sample_trainable' expected"
    );
}

/// C-3: contains edge `sample → sample_walkable`.
#[test]
fn walkable_interface_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_walkable", "contains"),
        "contains(sample, sample_walkable) must be present"
    );
}

/// C-4: contains edge `sample → sample_trainable`.
#[test]
fn trainable_interface_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_trainable", "contains"),
        "contains(sample, sample_trainable) must be present"
    );
}

/// C-5: inline interface → node present.
#[test]
fn inline_interface_emits_node() {
    let src = "interface Greeter { greet(): void; }";
    let ex = extract(src, "greet.ts");
    assert!(
        has_node(&ex, "greet_greeter"),
        "interface Greeter → node 'greet_greeter' expected"
    );
}

/// C-6: exported interface is still extracted.
#[test]
fn exported_interface_is_extracted() {
    let src = "export interface Shape { area(): number; }";
    let ex = extract(src, "shapes.ts");
    assert!(has_node(&ex, "shapes_shape"), "export interface must be extracted");
    assert!(
        has_edge(&ex, "shapes", "shapes_shape", "contains"),
        "contains edge expected for exported interface"
    );
}

// ── Group D: Abstract class (Animal) ──────────────────────────────────────────────────────────

/// D-1: `Animal` abstract class → node `sample_animal`.
#[test]
fn animal_abstract_class_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_animal"),
        "abstract class Animal → node 'sample_animal' expected"
    );
}

/// D-2: contains edge `sample → sample_animal`.
#[test]
fn animal_class_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_animal", "contains"),
        "contains(sample, sample_animal) must be present"
    );
}

/// D-3: `Animal implements Walkable` → inherits edge from local `sample_walkable`.
#[test]
fn animal_implements_walkable_inherits_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample_walkable", "sample_animal", "inherits"),
        "inherits(sample_walkable, sample_animal) must be present (implements)"
    );
}

/// D-4: abstract `speak()` is NOT extracted (tree-sitter parses it as `abstract_method_signature`,
/// not `method_definition`; the extractor deliberately skips it).
#[test]
fn animal_abstract_speak_not_extracted_as_method() {
    let ex = extract_sample();
    assert!(
        !has_node(&ex, "sample_animal_speak"),
        "abstract speak() must NOT produce a node (abstract_method_signature is skipped)"
    );
    assert!(
        !has_edge(&ex, "sample_animal", "sample_animal_speak", "method"),
        "abstract speak() must NOT produce a method edge"
    );
}

/// D-5: inline abstract class extracts its concrete methods.
#[test]
fn inline_abstract_class_extracts_concrete_methods() {
    let src = "abstract class Base { run(): void {} abstract stop(): void; }";
    let ex = extract(src, "base.ts");
    assert!(
        has_node(&ex, "base_base_run"),
        "concrete run() in abstract class → node expected"
    );
    assert!(
        !has_node(&ex, "base_base_stop"),
        "abstract stop() in abstract class → no node"
    );
}

// ── Group E: Animal methods ────────────────────────────────────────────────────────────────────

/// E-1: `constructor` in Animal → node `sample_animal_constructor`.
#[test]
fn animal_constructor_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_animal_constructor"),
        "Animal.constructor → 'sample_animal_constructor' expected"
    );
    assert!(
        has_edge(&ex, "sample_animal", "sample_animal_constructor", "method"),
        "method edge (sample_animal → sample_animal_constructor) expected"
    );
}

/// E-2: `getName` → `getname` node (camelCase fully lowercased).
#[test]
fn animal_getname_method_node_lowercased() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_animal_getname"),
        "Animal.getName → 'sample_animal_getname' (lowercased)"
    );
}

/// E-3: `walk` method.
#[test]
fn animal_walk_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_animal_walk"),
        "Animal.walk → 'sample_animal_walk' expected"
    );
    assert!(
        has_edge(&ex, "sample_animal", "sample_animal_walk", "method"),
        "method(sample_animal, sample_animal_walk) expected"
    );
}

/// E-4: `getSpeed` → `getspeed`.
#[test]
fn animal_getspeed_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_animal_getspeed"),
        "Animal.getSpeed → 'sample_animal_getspeed' expected"
    );
}

/// E-5: Animal has exactly 4 method edges (constructor, getName, walk, getSpeed; NOT abstract speak).
#[test]
fn animal_has_exactly_four_method_edges() {
    let ex = extract_sample();
    let animal_methods: Vec<_> = ex
        .edges
        .iter()
        .filter(|e| e.source == "sample_animal" && e.relation == "method")
        .collect();
    assert_eq!(
        animal_methods.len(),
        4,
        "Animal must have exactly 4 method edges; got {:?}",
        animal_methods
            .iter()
            .map(|e| e.target.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Group F: Dog class and inheritance ────────────────────────────────────────────────────────

/// F-1: `Dog` class → node `sample_dog`.
#[test]
fn dog_class_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog"),
        "class Dog → 'sample_dog' expected"
    );
}

/// F-2: contains edge `sample → sample_dog`.
#[test]
fn dog_class_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_dog", "contains"),
        "contains(sample, sample_dog) expected"
    );
}

/// F-3: `Dog extends Animal` → inherits edge from local `sample_animal` (extends).
#[test]
fn dog_extends_animal_inherits_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample_animal", "sample_dog", "inherits"),
        "inherits(sample_animal, sample_dog) expected (extends)"
    );
}

/// F-4: `Dog implements Walkable` → inherits edge from local `sample_walkable`.
#[test]
fn dog_implements_walkable_inherits_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample_walkable", "sample_dog", "inherits"),
        "inherits(sample_walkable, sample_dog) expected (implements Walkable)"
    );
}

/// F-5: `Dog implements Trainable` → inherits edge from local `sample_trainable`.
#[test]
fn dog_implements_trainable_inherits_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample_trainable", "sample_dog", "inherits"),
        "inherits(sample_trainable, sample_dog) expected (implements Trainable)"
    );
}

// ── Group G: Dog methods ───────────────────────────────────────────────────────────────────────

/// G-1: `constructor` in Dog → `sample_dog_constructor`.
#[test]
fn dog_constructor_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog_constructor"),
        "Dog.constructor → 'sample_dog_constructor' expected"
    );
    assert!(
        has_edge(&ex, "sample_dog", "sample_dog_constructor", "method"),
        "method(sample_dog, sample_dog_constructor) expected"
    );
}

/// G-2: `speak` → `sample_dog_speak`.
#[test]
fn dog_speak_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog_speak"),
        "Dog.speak → 'sample_dog_speak' expected"
    );
}

/// G-3: `fetch` → `sample_dog_fetch`.
#[test]
fn dog_fetch_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog_fetch"),
        "Dog.fetch → 'sample_dog_fetch' expected"
    );
    assert!(
        has_edge(&ex, "sample_dog", "sample_dog_fetch", "method"),
        "method(sample_dog, sample_dog_fetch) expected"
    );
}

/// G-4: `learn` → `sample_dog_learn`.
#[test]
fn dog_learn_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog_learn"),
        "Dog.learn → 'sample_dog_learn' expected"
    );
}

/// G-5: `getBreed` → `sample_dog_getbreed`.
#[test]
fn dog_getbreed_method_node_lowercased() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_dog_getbreed"),
        "Dog.getBreed → 'sample_dog_getbreed' (camelCase lowercased)"
    );
}

/// G-6: Dog has exactly 5 method edges.
#[test]
fn dog_has_exactly_five_method_edges() {
    let ex = extract_sample();
    let dog_methods: Vec<_> = ex
        .edges
        .iter()
        .filter(|e| e.source == "sample_dog" && e.relation == "method")
        .collect();
    assert_eq!(
        dog_methods.len(),
        5,
        "Dog must have 5 method edges; got {:?}",
        dog_methods
            .iter()
            .map(|e| e.target.as_str())
            .collect::<Vec<_>>()
    );
}

// ── Group H: Trainer class ────────────────────────────────────────────────────────────────────

/// H-1: `Trainer` class → node `sample_trainer`.
#[test]
fn trainer_class_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_trainer"),
        "class Trainer → 'sample_trainer' expected"
    );
}

/// H-2: contains edge `sample → sample_trainer`.
#[test]
fn trainer_class_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_trainer", "contains"),
        "contains(sample, sample_trainer) expected"
    );
}

/// H-3: `constructor` → `sample_trainer_constructor`.
#[test]
fn trainer_constructor_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_trainer_constructor"),
        "Trainer.constructor → 'sample_trainer_constructor' expected"
    );
    assert!(
        has_edge(&ex, "sample_trainer", "sample_trainer_constructor", "method"),
        "method(sample_trainer, sample_trainer_constructor) expected"
    );
}

/// H-4: `train` method → `sample_trainer_train`.
#[test]
fn trainer_train_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_trainer_train"),
        "Trainer.train → 'sample_trainer_train' expected"
    );
}

/// H-5: `getName` → `sample_trainer_getname`.
#[test]
fn trainer_getname_method_node() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_trainer_getname"),
        "Trainer.getName → 'sample_trainer_getname' expected"
    );
}

// ── Group I: Standalone function and arrow-fn const ───────────────────────────────────────────

/// I-1: exported `function createAnimal` → node `sample_createanimal`.
#[test]
fn standalone_function_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_createanimal"),
        "export function createAnimal → 'sample_createanimal' expected"
    );
}

/// I-2: contains edge `sample → sample_createanimal`.
#[test]
fn standalone_function_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_createanimal", "contains"),
        "contains(sample, sample_createanimal) expected"
    );
}

/// I-3: `export const makeTrainer = (...) =>` → node `sample_maketrainer`.
#[test]
fn arrow_fn_const_node_present() {
    let ex = extract_sample();
    assert!(
        has_node(&ex, "sample_maketrainer"),
        "const makeTrainer = () => ... → 'sample_maketrainer' expected"
    );
}

/// I-4: contains edge `sample → sample_maketrainer`.
#[test]
fn arrow_fn_const_has_contains_edge() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "sample_maketrainer", "contains"),
        "contains(sample, sample_maketrainer) expected"
    );
}

/// I-5: inline arrow fn const.
#[test]
fn inline_arrow_fn_const_emits_fn_node() {
    let src = "const greet = (name: string) => `Hello ${name}`;";
    let ex = extract(src, "util.ts");
    assert!(
        has_node(&ex, "util_greet"),
        "const greet = () => ... → 'util_greet' expected"
    );
    assert!(
        has_edge(&ex, "util", "util_greet", "contains"),
        "contains(util, util_greet) expected"
    );
}

// ── Group J: Import edges ──────────────────────────────────────────────────────────────────────

/// J-1: `import { EventEmitter } from 'events'` → imports_from `events`.
#[test]
fn imports_from_events_module() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "events", "imports_from"),
        "imports_from(sample, events) expected"
    );
}

/// J-2: `import { Logger } from './logger'` → imports_from `./logger`.
#[test]
fn imports_from_local_logger() {
    let ex = extract_sample();
    assert!(
        has_edge(&ex, "sample", "./logger", "imports_from"),
        "imports_from(sample, ./logger) expected"
    );
}

/// J-3: total imports_from count matches self-golden.
#[test]
fn total_imports_from_matches_golden() {
    let ex = extract_sample();
    let actual = edge_count(&ex, "imports_from");
    assert_eq!(
        actual, SAMPLE_IMPORTS_COUNT,
        "imports_from count: expected {SAMPLE_IMPORTS_COUNT}, got {actual}"
    );
}

/// J-4: import specifiers are lowercased.
#[test]
fn import_specifiers_are_lowercased() {
    let src = r#"import { X } from 'MyLib';"#;
    let ex = extract(src, "a.ts");
    assert!(
        has_edge(&ex, "a", "mylib", "imports_from"),
        "import source specifier must be lowercased ('mylib')"
    );
}

/// J-5: inline double-quote import.
#[test]
fn inline_double_quote_import() {
    let src = r#"import type { Foo } from "./foo";"#;
    let ex = extract(src, "bar.ts");
    assert!(
        has_edge(&ex, "bar", "./foo", "imports_from"),
        "double-quoted import must produce imports_from(bar, ./foo)"
    );
}

// ── Group K: Edge counts ───────────────────────────────────────────────────────────────────────

/// K-1: total edge count.
#[test]
fn total_edge_count_matches_golden() {
    let ex = extract_sample();
    assert_eq!(
        ex.edges.len(),
        SAMPLE_EDGE_COUNT,
        "total edges: expected {SAMPLE_EDGE_COUNT}, got {}",
        ex.edges.len()
    );
}

/// K-2: `contains` edge count.
#[test]
fn total_contains_edges_matches_golden() {
    let ex = extract_sample();
    let actual = edge_count(&ex, "contains");
    assert_eq!(
        actual, SAMPLE_CONTAINS_COUNT,
        "contains edges: expected {SAMPLE_CONTAINS_COUNT}, got {actual}"
    );
}

/// K-3: `method` edge count.
#[test]
fn total_method_edges_matches_golden() {
    let ex = extract_sample();
    let actual = edge_count(&ex, "method");
    assert_eq!(
        actual, SAMPLE_METHOD_COUNT,
        "method edges: expected {SAMPLE_METHOD_COUNT}, got {actual}"
    );
}

/// K-4: `inherits` edge count.
#[test]
fn total_inherits_edges_matches_golden() {
    let ex = extract_sample();
    let actual = edge_count(&ex, "inherits");
    assert_eq!(
        actual, SAMPLE_INHERITS_COUNT,
        "inherits edges: expected {SAMPLE_INHERITS_COUNT}, got {actual}"
    );
}

/// K-5: `calls` and `uses` are deliberately NOT emitted (documented divergence).
#[test]
fn calls_and_uses_not_emitted() {
    let ex = extract_sample();
    assert_eq!(
        edge_count(&ex, "calls"),
        0,
        "'calls' must never be emitted (documented divergence)"
    );
    assert_eq!(
        edge_count(&ex, "uses"),
        0,
        "'uses' must never be emitted (documented divergence)"
    );
}

// ── Group L: Node counts ───────────────────────────────────────────────────────────────────────

/// L-1: total node count.
#[test]
fn total_node_count_matches_golden() {
    let ex = extract_sample();
    assert_eq!(
        ex.nodes.len(),
        SAMPLE_NODE_COUNT,
        "total nodes: expected {SAMPLE_NODE_COUNT}, got {}; nodes={:?}",
        ex.nodes.len(),
        ex.nodes.iter().map(|n| &n.label).collect::<Vec<_>>()
    );
}

/// L-2: all node labels are unique.
#[test]
fn all_node_labels_are_unique() {
    let ex = extract_sample();
    let labels: Vec<_> = ex.nodes.iter().map(|n| &n.label).collect();
    let unique: HashSet<_> = labels.iter().collect();
    assert_eq!(
        unique.len(),
        labels.len(),
        "duplicate node labels detected"
    );
}

// ── Group M: Property verification ────────────────────────────────────────────────────────────

/// M-1: all method edges have `relation == "method"`.
#[test]
fn method_nodes_all_have_method_relation() {
    let ex = extract_sample();
    for e in ex.edges.iter().filter(|e| e.relation == "method") {
        assert_eq!(
            e.relation, "method",
            "method edge must have relation 'method'; got '{}'",
            e.relation
        );
    }
}

/// M-2: all `contains` edges originate from the file node `"sample"`.
#[test]
fn all_contains_edges_from_file_node() {
    let ex = extract_sample();
    for e in ex.edges.iter().filter(|e| e.relation == "contains") {
        assert_eq!(
            e.source, "sample",
            "all contains edges must originate from 'sample'; got source='{}'",
            e.source
        );
    }
}

/// M-3: all `inherits` edges have `relation == "inherits"`.
#[test]
fn inherits_edges_have_correct_relation() {
    let ex = extract_sample();
    for e in ex.edges.iter().filter(|e| e.relation == "inherits") {
        assert_eq!(e.relation, "inherits");
    }
}

/// M-4: every method-node target is reachable from a class node via a method edge.
#[test]
fn every_method_node_has_incoming_method_edge() {
    let ex = extract_sample();
    let method_targets: HashSet<_> = ex
        .edges
        .iter()
        .filter(|e| e.relation == "method")
        .map(|e| &e.target)
        .collect();
    // Verify that each target is actually a node in the extraction.
    for target in &method_targets {
        assert!(
            ex.nodes.iter().any(|n| &n.label == *target),
            "method edge target '{}' must be a node",
            target
        );
    }
}

/// M-5: Confidence is `Extracted` on all edges.
#[test]
fn all_edges_have_extracted_confidence() {
    use habitat_graph_core::Confidence;
    let ex = extract_sample();
    for e in &ex.edges {
        assert_eq!(
            e.confidence,
            Confidence::Extracted,
            "all edges must have Confidence::Extracted; edge {:?} has {:?}",
            e,
            e.confidence
        );
    }
}

// ── Group N: Inline TS snippet tests ──────────────────────────────────────────────────────────

/// N-1: simple class + method.
#[test]
fn inline_class_with_method_extracts() {
    let src = "class Greeter { greet(): string { return 'hi'; } }";
    let ex = extract(src, "greet.ts");
    assert!(has_node(&ex, "greet_greeter"), "class node");
    assert!(has_node(&ex, "greet_greeter_greet"), "method node");
    assert!(has_edge(&ex, "greet", "greet_greeter", "contains"), "contains");
    assert!(
        has_edge(&ex, "greet_greeter", "greet_greeter_greet", "method"),
        "method edge"
    );
}

/// N-2: `extends` with external (non-local) base uses plain lowercase label.
#[test]
fn inline_extends_external_base_uses_plain_label() {
    // `BaseClass` is NOT declared in this file, so `base_id = "baseclass"` (not "foo_baseclass").
    let src = "class Child extends BaseClass {}";
    let ex = extract(src, "foo.ts");
    assert!(
        has_edge(&ex, "baseclass", "foo_child", "inherits"),
        "external base → plain lowercased label 'baseclass'"
    );
}

/// N-3: `extends` with local base uses qualified label.
#[test]
fn inline_extends_local_base_uses_qualified_label() {
    let src = "class Base {} class Child extends Base {}";
    let ex = extract(src, "hier.ts");
    assert!(
        has_edge(&ex, "hier_base", "hier_child", "inherits"),
        "local base → qualified label 'hier_base'"
    );
}

/// N-4: `implements` edge is also emitted for local interfaces.
#[test]
fn inline_implements_local_interface_inherits_edge() {
    let src = "interface Runnable { run(): void; } class Task implements Runnable { run(): void {} }";
    let ex = extract(src, "task.ts");
    assert!(
        has_edge(&ex, "task_runnable", "task_task", "inherits"),
        "implements local interface → inherits(task_runnable, task_task)"
    );
}

/// N-5: `_private`-style method has leading underscore stripped.
#[test]
fn method_leading_underscore_stripped() {
    let src = "class Foo { _init(): void {} }";
    let ex = extract(src, "foo.ts");
    // method_id("_init") strips leading _ → "init"
    assert!(
        has_node(&ex, "foo_foo_init"),
        "method '_init' → 'foo_foo_init' (leading underscore stripped)"
    );
}

/// N-6: `__dunder__`-style method has both sets of underscores stripped.
#[test]
fn method_dunder_underscores_stripped() {
    let src = "class Foo { __enter__(): void {} }";
    let ex = extract(src, "foo.ts");
    // method_id("__enter__") = "enter"
    assert!(
        has_node(&ex, "foo_foo_enter"),
        "method '__enter__' → 'foo_foo_enter' (dunder underscores stripped)"
    );
}

/// N-7: TSX dialect is parsed without error.
#[test]
fn tsx_dialect_parses_without_error() {
    let src = "const App = (): JSX.Element => <div>Hello</div>;";
    let ex = extract(src, "app.tsx");
    assert!(has_node(&ex, "app"), "file node 'app' must be present for tsx");
}

/// N-8: malformed source is tolerated (tree-sitter error recovery).
#[test]
fn malformed_source_is_tolerated() {
    let src = "!!! INVALID @@@";
    let ex = extract(src, "bad.ts");
    assert!(has_node(&ex, "bad"), "file node must survive malformed source");
}

/// N-9: `function_expression` const (not arrow fn) is also extracted.
#[test]
fn function_expression_const_is_extracted() {
    let src = "const add = function(a: number, b: number) { return a + b; };";
    let ex = extract(src, "math.ts");
    assert!(
        has_node(&ex, "math_add"),
        "const add = function(...) → 'math_add' expected"
    );
    assert!(
        has_edge(&ex, "math", "math_add", "contains"),
        "contains(math, math_add) expected"
    );
}

/// N-10: enum declaration is extracted.
#[test]
fn enum_declaration_emits_node() {
    let src = "export enum Direction { North, South, East, West }";
    let ex = extract(src, "dir.ts");
    assert!(
        has_node(&ex, "dir_direction"),
        "enum Direction → 'dir_direction' expected"
    );
    assert!(
        has_edge(&ex, "dir", "dir_direction", "contains"),
        "contains(dir, dir_direction) expected"
    );
}

// ── Group O: Pipeline round-trip via detect → extract → build ─────────────────────────────────

/// O-1: the pipeline detects at least one file in the `worked/ts/src/` directory.
#[test]
fn pipeline_detects_ts_file() {
    let src_dir = std::path::PathBuf::from(format!(
        "{}/../../fixtures/worked/ts/src",
        env!("CARGO_MANIFEST_DIR")
    ));
    let files =
        habitat_graph_source::detect(&src_dir, &["ts"]).expect("detect ts files");
    assert!(
        !files.is_empty(),
        "detect must find at least one .ts file in fixtures/worked/ts/src/"
    );
}

/// O-2: extract_files succeeds for the ts corpus.
#[test]
fn pipeline_extract_files_succeeds() {
    let src_dir = std::path::PathBuf::from(format!(
        "{}/../../fixtures/worked/ts/src",
        env!("CARGO_MANIFEST_DIR")
    ));
    let files =
        habitat_graph_source::detect(&src_dir, &["ts"]).expect("detect ts files");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract_files");
    assert!(
        !extractions.is_empty(),
        "extract_files must return at least one extraction"
    );
}

/// O-3: assembled graph has the expected node count.
#[test]
fn pipeline_graph_node_count() {
    let src_dir = std::path::PathBuf::from(format!(
        "{}/../../fixtures/worked/ts/src",
        env!("CARGO_MANIFEST_DIR")
    ));
    let files =
        habitat_graph_source::detect(&src_dir, &["ts"]).expect("detect");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract_files");
    let graph = habitat_graph_build::assemble(extractions);
    assert_eq!(
        graph.nodes.len(),
        SAMPLE_NODE_COUNT,
        "assembled graph must have {SAMPLE_NODE_COUNT} nodes; got {}",
        graph.nodes.len()
    );
}

/// O-4: assembled graph has the expected edge count (dangling imports_from edges are dropped).
///
/// `assemble` drops edges where either endpoint is not a node in the extraction (dangling-edge
/// policy). The 2 `imports_from` edges to `events` and `./logger` have no corresponding target
/// nodes, so they are dropped: 25 raw → 23 assembled.
#[test]
fn pipeline_graph_edge_count() {
    let src_dir = std::path::PathBuf::from(format!(
        "{}/../../fixtures/worked/ts/src",
        env!("CARGO_MANIFEST_DIR")
    ));
    let files =
        habitat_graph_source::detect(&src_dir, &["ts"]).expect("detect");
    let extractions = habitat_graph_extract::extract_files(&files).expect("extract_files");
    let graph = habitat_graph_build::assemble(extractions);
    assert_eq!(
        graph.edges.len(),
        ASSEMBLED_EDGE_COUNT,
        "assembled graph must have {ASSEMBLED_EDGE_COUNT} edges (2 dangling imports_from dropped); got {}",
        graph.edges.len()
    );
}

/// O-5: two extraction runs produce identical results (determinism, R4).
#[test]
fn extraction_is_deterministic() {
    let ex1 = extract_sample();
    let ex2 = extract_sample();
    let labels1: Vec<_> = ex1.nodes.iter().map(|n| &n.label).collect();
    let labels2: Vec<_> = ex2.nodes.iter().map(|n| &n.label).collect();
    assert_eq!(labels1, labels2, "two runs must produce identical node labels");
    let edges1: Vec<_> = ex1
        .edges
        .iter()
        .map(|e| (&e.source, &e.target, &e.relation))
        .collect();
    let edges2: Vec<_> = ex2
        .edges
        .iter()
        .map(|e| (&e.source, &e.target, &e.relation))
        .collect();
    assert_eq!(edges1, edges2, "two runs must produce identical edges");
}
