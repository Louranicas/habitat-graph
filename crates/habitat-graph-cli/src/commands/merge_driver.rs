//! Git merge driver for `graph.json` (PC-tail) — wires [`habitat_graph_build::merge3`] into git's
//! `%O %A %B` calling convention so two branches' generated graphs auto-merge with no conflict.
//!
//! [`run_merge_driver`] is what git invokes (`habitat-graph merge-driver %O %A %B`): it reads the
//! base/ours/theirs `graph.json`, computes the deterministic 3-way merge, and writes the result
//! back to `ours` (git's `%A`, the file git keeps). [`install`] registers the driver in a repo
//! (`.gitattributes` + the `git config` lines). Because `graph.json` is canonically sorted (R4) the
//! merge is always conflict-free.

use std::path::Path;

use habitat_graph_core::{Graph, GraphError, Result};

/// The `.gitattributes` line that routes `graph.json` merges through this driver.
const GITATTRIBUTES_LINE: &str = "graph.json merge=habitat-graph";

/// Loads a node-link `graph.json` from `path` into a [`Graph`].
fn load(path: &Path) -> Result<Graph> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| GraphError::Io(format!("read {}: {e}", path.display())))?;
    habitat_graph_serve::from_node_link(&text)
}

/// Runs the 3-way merge git invokes as `habitat-graph merge-driver %O %A %B`.
///
/// Reads `base`/`ours`/`theirs`, writes the deterministic merge back to `ours` (git's `%A`).
/// Returns `0` on success (the merge is always conflict-free) or `1` on an I/O or parse error.
#[must_use]
pub fn run_merge_driver(base: &Path, ours: &Path, theirs: &Path) -> u8 {
    match merge3_to_ours(base, ours, theirs) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("habitat-graph merge-driver: {e}");
            1
        }
    }
}

/// Computes `merge3(base, ours, theirs)` and writes the canonical result to `ours`.
fn merge3_to_ours(base: &Path, ours: &Path, theirs: &Path) -> Result<()> {
    let merged = habitat_graph_build::merge3(&load(base)?, &load(ours)?, &load(theirs)?);
    let json = habitat_graph_export::to_node_link(&merged)?;
    std::fs::write(ours, json)
        .map_err(|e| GraphError::Io(format!("write {}: {e}", ours.display())))?;
    Ok(())
}

/// Installs the merge driver for `graph.json` in the repo at `repo`.
///
/// Appends the `.gitattributes` line (idempotently) and prints the two `git config` commands that
/// complete registration. Returns `0` on success or `1` on an I/O error.
#[must_use]
pub fn install(repo: &Path) -> u8 {
    match install_gitattributes(repo) {
        Ok(added) => {
            if added {
                println!("wrote {GITATTRIBUTES_LINE:?} to .gitattributes");
            } else {
                println!(".gitattributes already registers the merge driver");
            }
            println!("complete setup by running, inside the repo:");
            println!(
                "  git config merge.habitat-graph.name 'habitat-graph deterministic graph.json merge'"
            );
            println!(
                "  git config merge.habitat-graph.driver 'habitat-graph merge-driver %O %A %B'"
            );
            0
        }
        Err(e) => {
            eprintln!("habitat-graph install-merge-driver: {e}");
            1
        }
    }
}

/// Appends [`GITATTRIBUTES_LINE`] to `<repo>/.gitattributes` if absent. Returns whether it added it.
fn install_gitattributes(repo: &Path) -> Result<bool> {
    let path = repo.join(".gitattributes");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == GITATTRIBUTES_LINE) {
        return Ok(false);
    }
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(GITATTRIBUTES_LINE);
    content.push('\n');
    std::fs::write(&path, content)
        .map_err(|e| GraphError::Io(format!("write {}: {e}", path.display())))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    use super::{install, install_gitattributes, run_merge_driver, GITATTRIBUTES_LINE};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// A unique temp directory per call (atomic counter + pid) so parallel tests never collide.
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    fn tdir() -> PathBuf {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("hg_md_{pid}_{seq}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A directory with conventional base/ours/theirs `graph.json` paths.
    struct Trio {
        dir: PathBuf,
        base: PathBuf,
        ours: PathBuf,
        theirs: PathBuf,
    }
    fn trio() -> Trio {
        let dir = tdir();
        Trio {
            base: dir.join("base.json"),
            ours: dir.join("ours.json"),
            theirs: dir.join("theirs.json"),
            dir,
        }
    }

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "a.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn graph_with(labels: &[&str]) -> Graph {
        let mut graph = Graph::new();
        for (idx, label) in labels.iter().enumerate() {
            graph
                .nodes
                .push(node(u32::try_from(idx).expect("small index"), label));
        }
        graph
    }

    fn write_g(path: &Path, graph: &Graph) {
        let json = habitat_graph_export::to_node_link(graph).expect("serialize");
        fs::write(path, json).expect("write graph");
    }

    fn write_labels(path: &Path, labels: &[&str]) {
        write_g(path, &graph_with(labels));
    }

    fn write_raw(path: &Path, raw: &str) {
        fs::write(path, raw).expect("write raw");
    }

    fn load_graph(path: &Path) -> Graph {
        let text = fs::read_to_string(path).expect("read");
        habitat_graph_serve::from_node_link(&text).expect("parse")
    }

    fn labels_in(path: &Path) -> BTreeSet<String> {
        load_graph(path)
            .nodes
            .iter()
            .map(|n| n.label.clone())
            .collect()
    }

    /// Writes base/ours/theirs from label lists, runs the driver, returns (exit, merged-labels).
    fn run_labels(base: &[&str], ours: &[&str], theirs: &[&str]) -> (u8, BTreeSet<String>) {
        let t = trio();
        write_labels(&t.base, base);
        write_labels(&t.ours, ours);
        write_labels(&t.theirs, theirs);
        let code = run_merge_driver(&t.base, &t.ours, &t.theirs);
        let merged = if code == 0 {
            labels_in(&t.ours)
        } else {
            BTreeSet::new()
        };
        (code, merged)
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    // ── Merge semantics through the file boundary ─────────────────────────────

    #[test]
    fn union_both_add_distinct() {
        let (code, labels) = run_labels(&[], &["A"], &["B"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["A", "B"]));
    }

    #[test]
    fn ours_only_addition_kept() {
        let (code, labels) = run_labels(&[], &["A"], &[]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["A"]));
    }

    #[test]
    fn theirs_only_addition_kept() {
        let (code, labels) = run_labels(&[], &[], &["B"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["B"]));
    }

    #[test]
    fn both_add_same_label_deduped() {
        let (code, labels) = run_labels(&[], &["Same"], &["Same"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Same"]));
    }

    #[test]
    fn deletion_on_ours_respected() {
        let (code, labels) = run_labels(&["X", "Keep"], &["Keep"], &["X", "Keep"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Keep"]));
    }

    #[test]
    fn deletion_on_theirs_respected() {
        let (code, labels) = run_labels(&["X", "Keep"], &["X", "Keep"], &["Keep"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Keep"]));
    }

    #[test]
    fn kept_on_both_sides_survives() {
        let (code, labels) = run_labels(&["Z"], &["Z"], &["Z"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Z"]));
    }

    #[test]
    fn deleted_on_both_sides_gone() {
        let (code, labels) = run_labels(&["Gone", "Stay"], &["Stay"], &["Stay"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Stay"]));
    }

    #[test]
    fn each_side_deletes_a_different_base_node() {
        let (code, labels) = run_labels(&["X", "Y", "Z"], &["Y", "Z"], &["X", "Z"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["Z"]));
    }

    #[test]
    fn addition_and_deletion_mixed() {
        let (code, labels) = run_labels(&["B"], &["B", "O"], &[]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["O"]));
    }

    #[test]
    fn all_empty_yields_empty_graph() {
        let (code, labels) = run_labels(&[], &[], &[]);
        assert_eq!(code, 0);
        assert!(labels.is_empty());
    }

    #[test]
    fn self_merge_unchanged() {
        let (code, labels) = run_labels(&["A", "B", "C"], &["A", "B", "C"], &["A", "B", "C"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["A", "B", "C"]));
    }

    #[test]
    fn many_nodes_union() {
        let many: Vec<&str> = vec!["n0", "n1", "n2", "n3", "n4", "n5", "n6", "n7", "n8", "n9"];
        let (code, labels) = run_labels(&[], &many[..5], &many[5..]);
        assert_eq!(code, 0);
        assert_eq!(labels.len(), 10);
    }

    #[test]
    fn empty_ours_populated_theirs() {
        let (code, labels) = run_labels(&[], &[], &["only_theirs"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["only_theirs"]));
    }

    #[test]
    fn populated_ours_empty_theirs() {
        let (code, labels) = run_labels(&[], &["only_ours"], &[]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["only_ours"]));
    }

    #[test]
    fn three_way_with_no_base_is_pure_union() {
        let (code, labels) = run_labels(&[], &["a", "b"], &["b", "c"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["a", "b", "c"]));
    }

    #[test]
    fn large_base_partial_delete_each_side() {
        let base = ["a", "b", "c", "d", "e", "f"];
        // Only c,d are kept on BOTH sides; a,b,e,f each deleted on exactly one side.
        let (code, labels) = run_labels(&base, &["a", "b", "c", "d"], &["c", "d", "e", "f"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["c", "d"]));
    }

    #[test]
    fn new_labels_from_both_sides_all_kept() {
        let (code, labels) = run_labels(&["base"], &["base", "o1", "o2"], &["base", "t1", "t2"]);
        assert_eq!(code, 0);
        assert_eq!(labels, set(&["base", "o1", "o2", "t1", "t2"]));
    }

    // ── Output goes to OURS, base/theirs untouched, determinism ───────────────

    #[test]
    fn result_written_to_ours_not_theirs() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        let theirs_before = fs::read(&t.theirs).expect("read theirs");
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        assert_eq!(labels_in(&t.ours), set(&["A", "B"]));
        assert_eq!(fs::read(&t.theirs).expect("read theirs"), theirs_before);
    }

    #[test]
    fn run_does_not_touch_base_file() {
        let t = trio();
        write_labels(&t.base, &["base"]);
        write_labels(&t.ours, &["base", "x"]);
        write_labels(&t.theirs, &["base", "y"]);
        let base_before = fs::read(&t.base).expect("read base");
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        assert_eq!(fs::read(&t.base).expect("read base"), base_before);
    }

    #[test]
    fn determinism_two_runs_byte_identical() {
        let t = trio();
        let ours2 = t.dir.join("o2.json");
        write_labels(&t.base, &["base"]);
        write_labels(&t.ours, &["base", "x", "y"]);
        write_labels(&ours2, &["base", "x", "y"]);
        write_labels(&t.theirs, &["base", "z"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        assert_eq!(run_merge_driver(&t.base, &ours2, &t.theirs), 0);
        assert_eq!(
            fs::read(&t.ours).expect("read ours"),
            fs::read(&ours2).expect("read ours2")
        );
    }

    #[test]
    fn merged_output_nodes_sorted_by_id() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.ours, &["m", "a", "z"]);
        write_labels(&t.theirs, &["q", "b"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        let ids: Vec<u32> = load_graph(&t.ours)
            .nodes
            .iter()
            .map(|n| n.id.get())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "R4: nodes sorted by id");
    }

    #[test]
    fn merged_file_is_valid_reloadable_graph() {
        let (code, _) = run_labels(&[], &["A"], &["B"]);
        assert_eq!(code, 0); // run_labels re-parses ours; a clean parse means valid graph.json.
    }

    // ── Edges through the merge ───────────────────────────────────────────────

    fn graph_with_edge() -> Graph {
        let mut graph = Graph::new();
        graph.nodes.push(node(0, "Caller"));
        graph.nodes.push(node(1, "Callee"));
        graph.edges.push(Edge {
            source: NodeId::new(0),
            target: NodeId::new(1),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        });
        graph
    }

    #[test]
    fn edge_survives_merge() {
        let t = trio();
        write_g(&t.base, &Graph::new());
        write_g(&t.ours, &graph_with_edge());
        write_g(&t.theirs, &Graph::new());
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        assert!(load_graph(&t.ours)
            .edges
            .iter()
            .any(|e| e.relation == "calls"));
    }

    #[test]
    fn edge_confidence_preserved_through_merge() {
        let t = trio();
        write_g(&t.base, &Graph::new());
        write_g(&t.ours, &graph_with_edge());
        write_g(&t.theirs, &Graph::new());
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        assert_eq!(
            load_graph(&t.ours).edges[0].confidence,
            Confidence::Extracted
        );
    }

    #[test]
    fn edge_with_endpoints_both_deleted_is_dropped() {
        let t = trio();
        write_g(&t.base, &graph_with_edge());
        write_g(&t.ours, &Graph::new());
        write_g(&t.theirs, &Graph::new());
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        let merged = load_graph(&t.ours);
        assert!(merged.edges.is_empty());
        assert!(merged.nodes.is_empty());
    }

    #[test]
    fn ours_data_wins_on_shared_label() {
        let t = trio();
        write_g(&t.base, &Graph::new());
        let mut ours = Graph::new();
        ours.nodes.push(Node {
            id: NodeId::new(0),
            label: "S".to_owned(),
            source_file: "ours.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        });
        let mut theirs = Graph::new();
        theirs.nodes.push(Node {
            id: NodeId::new(0),
            label: "S".to_owned(),
            source_file: "theirs.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        });
        write_g(&t.ours, &ours);
        write_g(&t.theirs, &theirs);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 0);
        let merged = load_graph(&t.ours);
        let shared = merged
            .nodes
            .iter()
            .find(|n| n.label == "S")
            .expect("S present");
        assert_eq!(
            shared.source_file, "ours.rs",
            "ours data wins on shared label"
        );
    }

    // ── Error paths — every one returns exit code 1 ───────────────────────────

    #[test]
    fn missing_base_returns_one() {
        let t = trio();
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(
            run_merge_driver(&t.dir.join("absent.json"), &t.ours, &t.theirs),
            1
        );
    }

    #[test]
    fn missing_ours_returns_one() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(
            run_merge_driver(&t.base, &t.dir.join("absent.json"), &t.theirs),
            1
        );
    }

    #[test]
    fn missing_theirs_returns_one() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.ours, &["A"]);
        assert_eq!(
            run_merge_driver(&t.base, &t.ours, &t.dir.join("absent.json")),
            1
        );
    }

    #[test]
    fn all_three_missing_returns_one() {
        let t = trio();
        let absent = t.dir.join("absent.json");
        assert_eq!(run_merge_driver(&absent, &absent, &absent), 1);
    }

    #[test]
    fn malformed_json_base_returns_one() {
        let t = trio();
        write_raw(&t.base, "{ this is not json");
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn malformed_json_ours_returns_one() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_raw(&t.ours, "not json at all");
        write_labels(&t.theirs, &["B"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn malformed_json_theirs_returns_one() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.ours, &["A"]);
        write_raw(&t.theirs, "</xml>");
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn empty_file_returns_one() {
        let t = trio();
        write_raw(&t.base, "");
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn valid_json_wrong_shape_returns_one() {
        let t = trio();
        write_raw(&t.base, "{\"unrelated\": 42}");
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn base_is_directory_returns_one() {
        let t = trio();
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        // `t.dir` itself is a directory; reading it as a file fails → exit 1.
        assert_eq!(run_merge_driver(&t.dir, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn non_utf8_file_returns_one() {
        let t = trio();
        fs::write(&t.base, [0xff_u8, 0xfe, 0x00, 0x9f]).expect("write bytes");
        write_labels(&t.ours, &["A"]);
        write_labels(&t.theirs, &["B"]);
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
    }

    #[test]
    fn ours_unchanged_on_error() {
        let t = trio();
        write_labels(&t.base, &[]);
        write_labels(&t.ours, &["A"]);
        let ours_before = fs::read(&t.ours).expect("read ours");
        write_raw(&t.theirs, "broken");
        assert_eq!(run_merge_driver(&t.base, &t.ours, &t.theirs), 1);
        assert_eq!(fs::read(&t.ours).expect("read ours"), ours_before);
    }

    // ── install_gitattributes ─────────────────────────────────────────────────

    #[test]
    fn install_creates_gitattributes_when_absent() {
        let dir = tdir();
        assert!(install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert!(ga.contains(GITATTRIBUTES_LINE));
    }

    #[test]
    fn install_returns_true_then_false_idempotent() {
        let dir = tdir();
        assert!(install_gitattributes(&dir).expect("first"));
        assert!(!install_gitattributes(&dir).expect("second"));
    }

    #[test]
    fn install_already_present_returns_false() {
        let dir = tdir();
        fs::write(
            dir.join(".gitattributes"),
            format!("{GITATTRIBUTES_LINE}\n"),
        )
        .expect("seed");
        assert!(!install_gitattributes(&dir).expect("install"));
    }

    #[test]
    fn install_recognizes_line_with_surrounding_whitespace() {
        let dir = tdir();
        fs::write(
            dir.join(".gitattributes"),
            format!("  {GITATTRIBUTES_LINE}  \n"),
        )
        .expect("seed");
        assert!(!install_gitattributes(&dir).expect("install"));
    }

    #[test]
    fn install_preserves_existing_content_no_trailing_newline() {
        let dir = tdir();
        fs::write(dir.join(".gitattributes"), "*.rs text").expect("seed");
        assert!(install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert!(
            ga.contains("*.rs text"),
            "must not clobber pre-existing rules"
        );
        assert!(ga.contains(GITATTRIBUTES_LINE));
        assert!(
            ga.contains("text\ngraph.json"),
            "newline inserted before new line"
        );
    }

    #[test]
    fn install_preserves_existing_content_with_trailing_newline() {
        let dir = tdir();
        fs::write(dir.join(".gitattributes"), "*.rs text\n").expect("seed");
        assert!(install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert!(ga.contains("*.rs text"));
        assert!(ga.contains(GITATTRIBUTES_LINE));
        assert!(!ga.contains("text\n\n"), "no spurious blank line");
    }

    #[test]
    fn install_appended_line_ends_with_newline() {
        let dir = tdir();
        assert!(install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert!(ga.ends_with('\n'));
    }

    #[test]
    fn install_does_not_duplicate_on_existing_multiline() {
        let dir = tdir();
        fs::write(
            dir.join(".gitattributes"),
            format!("*.rs text\n{GITATTRIBUTES_LINE}\n*.md text\n"),
        )
        .expect("seed");
        assert!(!install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert_eq!(
            ga.matches(GITATTRIBUTES_LINE).count(),
            1,
            "no duplicate line"
        );
    }

    #[test]
    fn install_empty_file_writes_single_line() {
        let dir = tdir();
        fs::write(dir.join(".gitattributes"), "").expect("seed");
        assert!(install_gitattributes(&dir).expect("install"));
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert_eq!(ga, format!("{GITATTRIBUTES_LINE}\n"));
    }

    // ── install() command wrapper ─────────────────────────────────────────────

    #[test]
    fn install_command_returns_zero_fresh() {
        let dir = tdir();
        assert_eq!(install(&dir), 0);
    }

    #[test]
    fn install_command_returns_zero_idempotent() {
        let dir = tdir();
        assert_eq!(install(&dir), 0);
        assert_eq!(install(&dir), 0);
    }

    #[test]
    fn install_command_returns_zero_with_existing_content() {
        let dir = tdir();
        fs::write(dir.join(".gitattributes"), "*.png binary\n").expect("seed");
        assert_eq!(install(&dir), 0);
        let ga = fs::read_to_string(dir.join(".gitattributes")).expect("read");
        assert!(ga.contains("*.png binary") && ga.contains(GITATTRIBUTES_LINE));
    }

    // ── Constant + exit-code contract ─────────────────────────────────────────

    #[test]
    fn success_returns_zero() {
        let (code, _) = run_labels(&[], &["A"], &["B"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn the_gitattributes_constant_targets_graph_json() {
        assert!(GITATTRIBUTES_LINE.starts_with("graph.json "));
        assert!(GITATTRIBUTES_LINE.contains("merge=habitat-graph"));
    }
}
