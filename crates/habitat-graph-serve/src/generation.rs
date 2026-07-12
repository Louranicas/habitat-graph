//! Content-addressed generation id (FO-5) — a stable cache key for every response.
//!
//! [`generation_id`] returns a deterministic short content hash of the [`Graph`]. An agent
//! includes it as a cache key: identical graphs produce identical ids, so a client can skip
//! re-fetching when the graph has not changed. Any structural mutation changes the id.
//!
//! # Canonicalisation
//!
//! The hash is insertion-order independent (R4). Collections are sorted before hashing: nodes by
//! [`NodeId`](habitat_graph_core::NodeId), edges by `(source, target, relation, confidence)`, and
//! communities by [`CommunityId`](habitat_graph_core::CommunityId) (with each community's member
//! list also sorted ascending).
//!
//! Variable-length fields (strings and member lists) are length-prefixed with a `u64`
//! little-endian count so that `"ab"` and `("a", "b")` do not hash to the same bytes
//! (concatenation-collision prevention). Fixed domain-separator tags (`NODES\0`, `EDGES\0`,
//! `COMMS\0`) guard section boundaries so node data cannot be confused with edge data.
//!
//! The output is the first [`GENERATION_LEN`] lowercase hex chars of the `blake3` digest.

use habitat_graph_core::{Community, Confidence, Edge, Graph, Node};

/// Length in hex characters of the returned generation id.
pub const GENERATION_LEN: usize = 16;

/// Domain separator prepended before the entire nodes section.
const TAG_NODES: &[u8] = b"NODES\x00";
/// Domain separator prepended before the entire edges section.
const TAG_EDGES: &[u8] = b"EDGES\x00";
/// Domain separator prepended before the entire communities section.
const TAG_COMMS: &[u8] = b"COMMS\x00";
/// Per-record separator for individual node records.
const TAG_NODE: &[u8] = b"N\x00";
/// Per-record separator for individual edge records.
const TAG_EDGE: &[u8] = b"E\x00";
/// Per-record separator for individual community records.
const TAG_COMM: &[u8] = b"C\x00";

/// Returns a deterministic short content-hash of `graph` (the `generation` id).
///
/// Equal graphs hash equally (R4); any change to the node set, edge set, or community set changes
/// the returned id. The hash is insertion-order independent because all collections are sorted
/// before hashing. Variable-length string fields are length-prefixed to prevent concatenation
/// collisions (e.g. `label="ab"` + `source_file="c"` hashes differently from `label="a"` +
/// `source_file="bc"`).
///
/// The returned string is exactly [`GENERATION_LEN`] lowercase hex characters.
#[must_use]
pub fn generation_id(graph: &Graph) -> String {
    let mut hasher = blake3::Hasher::new();

    // ── nodes section ──────────────────────────────────────────────────────────
    hasher.update(TAG_NODES);
    let mut nodes: Vec<&Node> = graph.nodes.iter().collect();
    nodes.sort_unstable_by_key(|n| n.id);
    // usize fits in u64 on all supported targets (16/32/64-bit); the cast is always lossless.
    hasher.update(&(nodes.len() as u64).to_le_bytes());
    for node in nodes {
        hash_node(&mut hasher, node);
    }

    // ── edges section ──────────────────────────────────────────────────────────
    hasher.update(TAG_EDGES);
    let mut edges: Vec<&Edge> = graph.edges.iter().collect();
    edges.sort_unstable_by(|a, b| {
        (
            a.source,
            a.target,
            a.relation.as_str(),
            confidence_ord(a.confidence),
        )
            .cmp(&(
                b.source,
                b.target,
                b.relation.as_str(),
                confidence_ord(b.confidence),
            ))
    });
    hasher.update(&(edges.len() as u64).to_le_bytes());
    for edge in edges {
        hash_edge(&mut hasher, edge);
    }

    // ── communities section ────────────────────────────────────────────────────
    hasher.update(TAG_COMMS);
    let mut comms: Vec<&Community> = graph.communities.iter().collect();
    comms.sort_unstable_by_key(|c| c.id);
    hasher.update(&(comms.len() as u64).to_le_bytes());
    for comm in comms {
        hash_community(&mut hasher, comm);
    }

    hasher
        .finalize()
        .to_hex()
        .chars()
        .take(GENERATION_LEN)
        .collect()
}

/// Maps a [`Confidence`] variant to a fixed byte discriminant for hashing.
///
/// The mapping `Extracted=0`, `Inferred=1`, `Ambiguous=2` is explicit and stable rather than
/// relying on the internal enum discriminant, ensuring the byte representation never changes even
/// if the enum order is refactored.
#[inline]
const fn confidence_ord(c: Confidence) -> u8 {
    match c {
        Confidence::Extracted => 0,
        Confidence::Inferred => 1,
        Confidence::Ambiguous => 2,
    }
}

/// Feeds one [`Node`]'s fields into `hasher` using domain-tagged, length-prefixed encoding.
fn hash_node(hasher: &mut blake3::Hasher, node: &Node) {
    hasher.update(TAG_NODE);
    // Fixed-width NodeId — 4 bytes, no length prefix needed.
    hasher.update(&node.id.get().to_le_bytes());
    // Variable-width string fields — length-prefixed to prevent concatenation collisions.
    hash_str(hasher, &node.label);
    hash_str(hasher, &node.source_file);
    // Fixed-width Span — four u32 fields (16 bytes total).
    hasher.update(&node.source_location.start_byte.to_le_bytes());
    hasher.update(&node.source_location.end_byte.to_le_bytes());
    hasher.update(&node.source_location.start_line.to_le_bytes());
    hasher.update(&node.source_location.end_line.to_le_bytes());
}

/// Feeds one [`Edge`]'s fields into `hasher` using domain-tagged, length-prefixed encoding.
fn hash_edge(hasher: &mut blake3::Hasher, edge: &Edge) {
    hasher.update(TAG_EDGE);
    // Fixed-width NodeId source and target — 4 bytes each.
    hasher.update(&edge.source.get().to_le_bytes());
    hasher.update(&edge.target.get().to_le_bytes());
    // Variable-width relation string — length-prefixed.
    hash_str(hasher, &edge.relation);
    // Confidence — fixed 1-byte discriminant; no length prefix needed.
    hasher.update(&[confidence_ord(edge.confidence)]);
}

/// Feeds one [`Community`]'s fields into `hasher`, sorting members for insertion-order
/// independence.
fn hash_community(hasher: &mut blake3::Hasher, comm: &Community) {
    hasher.update(TAG_COMM);
    // Fixed-width CommunityId — 4 bytes.
    hasher.update(&comm.id.get().to_le_bytes());
    // Variable-width label — length-prefixed.
    hash_str(hasher, &comm.label);
    // Sort members before hashing so the id is independent of member insertion order.
    let mut members: Vec<u32> = comm.members.iter().map(|m| m.get()).collect();
    members.sort_unstable();
    hasher.update(&(members.len() as u64).to_le_bytes());
    for member in members {
        hasher.update(&member.to_le_bytes());
    }
}

/// Hashes a string as a `u64` little-endian byte-length prefix followed by the UTF-8 bytes.
///
/// The length prefix ensures that `"ab"` and `("a", "b")` — two different strings concatenated —
/// are not confused with a single string `"ab"`, preventing concatenation collisions across field
/// boundaries.
#[inline]
fn hash_str(hasher: &mut blake3::Hasher, s: &str) {
    let bytes = s.as_bytes();
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use habitat_graph_core::{Community, CommunityId, Confidence, Edge, Graph, Node, NodeId, Span};

    use super::{generation_id, GENERATION_LEN};

    // ── test helpers ─────────────────────────────────────────────────────────

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "a.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }

    fn node_full(id: u32, label: &str, file: &str, span: Span) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span,
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str, conf: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: conf,
        }
    }

    fn community(id: u32, label: &str, members: Vec<u32>) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: members.into_iter().map(NodeId::new).collect(),
        }
    }

    // ── Category 1: Output format invariants ─────────────────────────────────

    #[test]
    fn generation_len_const_is_16() {
        assert_eq!(GENERATION_LEN, 16);
    }

    #[test]
    fn empty_graph_produces_fixed_length_hex() {
        let id = generation_id(&Graph::new());
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn single_node_graph_produces_fixed_length_hex() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "alpha"));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn all_sections_populated_produces_fixed_length_hex() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "n1"));
        g.edges.push(edge(1, 1, "self", Confidence::Extracted));
        g.communities.push(community(0, "c0", vec![1]));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn large_graph_produces_fixed_length_hex() {
        let mut g = Graph::new();
        for i in 0..100_u32 {
            g.nodes.push(node(i, &format!("node_{i}")));
            if i > 0 {
                g.edges.push(edge(i - 1, i, "next", Confidence::Inferred));
            }
        }
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ── Category 2: Determinism ───────────────────────────────────────────────

    #[test]
    fn deterministic_empty_graph() {
        assert_eq!(generation_id(&Graph::new()), generation_id(&Graph::new()));
    }

    #[test]
    fn deterministic_single_node() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "a"));
        assert_eq!(generation_id(&g), generation_id(&g));
    }

    #[test]
    fn deterministic_with_edges_and_communities() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "n1"));
        g.nodes.push(node(2, "n2"));
        g.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        g.communities.push(community(0, "cluster", vec![1, 2]));
        assert_eq!(generation_id(&g), generation_id(&g));
    }

    #[test]
    fn deterministic_repeated_calls() {
        let mut g = Graph::new();
        for i in 0..10_u32 {
            g.nodes.push(node(i, &format!("n{i}")));
        }
        let first = generation_id(&g);
        for _ in 0..9 {
            assert_eq!(generation_id(&g), first);
        }
    }

    // ── Category 3: Insertion-order independence — nodes ──────────────────────

    #[test]
    fn nodes_insertion_order_independent_two() {
        let mut a = Graph::new();
        a.nodes.push(node(1, "alpha"));
        a.nodes.push(node(2, "beta"));

        let mut b = Graph::new();
        b.nodes.push(node(2, "beta"));
        b.nodes.push(node(1, "alpha"));

        assert_eq!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn nodes_insertion_order_independent_three_permutations() {
        let build = |order: [usize; 3]| {
            let pairs: [(u32, &str); 3] = [(1, "alpha"), (2, "beta"), (3, "gamma")];
            let mut g = Graph::new();
            for &i in &order {
                let (id, label) = pairs[i];
                g.nodes.push(node(id, label));
            }
            g
        };
        let canonical = generation_id(&build([0, 1, 2]));
        assert_eq!(canonical, generation_id(&build([2, 0, 1])));
        assert_eq!(canonical, generation_id(&build([1, 2, 0])));
        assert_eq!(canonical, generation_id(&build([2, 1, 0])));
    }

    // ── Category 4: Insertion-order independence — edges ─────────────────────

    #[test]
    fn edges_insertion_order_independent_two() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        a.edges.push(edge(3, 4, "imports", Confidence::Inferred));

        let mut b = Graph::new();
        b.edges.push(edge(3, 4, "imports", Confidence::Inferred));
        b.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        assert_eq!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn edges_insertion_order_independent_three() {
        let e1 = || edge(1, 2, "calls", Confidence::Extracted);
        let e2 = || edge(2, 3, "imports", Confidence::Inferred);
        let e3 = || edge(1, 3, "defines", Confidence::Ambiguous);

        let mut a = Graph::new();
        a.edges.extend([e1(), e2(), e3()]);

        let mut b = Graph::new();
        b.edges.extend([e3(), e1(), e2()]);

        assert_eq!(generation_id(&a), generation_id(&b));
    }

    // ── Category 5: Insertion-order independence — communities ────────────────

    #[test]
    fn communities_insertion_order_independent_two() {
        let mut a = Graph::new();
        a.communities.push(community(0, "first", vec![1, 2]));
        a.communities.push(community(1, "second", vec![3, 4]));

        let mut b = Graph::new();
        b.communities.push(community(1, "second", vec![3, 4]));
        b.communities.push(community(0, "first", vec![1, 2]));

        assert_eq!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn community_members_insertion_order_independent() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c", vec![1, 2, 3]));

        let mut b = Graph::new();
        b.communities.push(community(0, "c", vec![3, 1, 2]));

        assert_eq!(generation_id(&a), generation_id(&b));
    }

    // ── Category 6: Node change sensitivity ──────────────────────────────────

    #[test]
    fn changes_when_node_added() {
        let empty = generation_id(&Graph::new());
        let mut g = Graph::new();
        g.nodes.push(node(1, "x"));
        assert_ne!(empty, generation_id(&g));
    }

    #[test]
    fn changes_when_node_removed() {
        let mut a = Graph::new();
        a.nodes.push(node(1, "x"));
        a.nodes.push(node(2, "y"));

        let mut b = Graph::new();
        b.nodes.push(node(1, "x"));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_label_changed() {
        let mut a = Graph::new();
        a.nodes.push(node(1, "alpha"));

        let mut b = Graph::new();
        b.nodes.push(node(1, "beta"));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_source_file_changed() {
        let mut a = Graph::new();
        a.nodes
            .push(node_full(1, "n", "src/a.rs", Span::new(0, 1, 1, 1)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "src/b.rs", Span::new(0, 1, 1, 1)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_start_byte_changed() {
        let mut a = Graph::new();
        a.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 1, 1)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "f.rs", Span::new(1, 10, 1, 1)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_end_byte_changed() {
        let mut a = Graph::new();
        a.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 1, 1)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 11, 1, 1)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_start_line_changed() {
        let mut a = Graph::new();
        a.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 1, 2)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 2, 2)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_end_line_changed() {
        let mut a = Graph::new();
        a.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 1, 1)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "f.rs", Span::new(0, 10, 1, 2)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_node_id_changed() {
        // Same label but different NodeId — the id is part of the canonical form.
        let mut a = Graph::new();
        a.nodes.push(node(1, "same_label"));

        let mut b = Graph::new();
        b.nodes.push(node(2, "same_label"));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    // ── Category 7: Edge change sensitivity ──────────────────────────────────

    #[test]
    fn changes_when_edge_added() {
        let mut a = Graph::new();
        a.nodes.push(node(1, "a"));
        a.nodes.push(node(2, "b"));
        let base = generation_id(&a);

        let mut b = a.clone();
        b.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        assert_ne!(base, generation_id(&b));
    }

    #[test]
    fn changes_when_edge_removed() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "calls", Confidence::Extracted));
        a.edges.push(edge(2, 3, "imports", Confidence::Inferred));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_edge_source_changed() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(9, 2, "calls", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_edge_target_changed() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 9, "calls", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_edge_relation_changed() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "imports", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_confidence_extracted_to_inferred() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "rel", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "rel", Confidence::Inferred));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_confidence_inferred_to_ambiguous() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "rel", Confidence::Inferred));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "rel", Confidence::Ambiguous));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_confidence_extracted_to_ambiguous() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "rel", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "rel", Confidence::Ambiguous));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    // ── Category 8: Community change sensitivity ──────────────────────────────

    #[test]
    fn changes_when_community_added() {
        let a = Graph::new();
        let mut b = Graph::new();
        b.communities.push(community(0, "new_cluster", vec![1]));
        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_community_removed() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c1", vec![1]));
        a.communities.push(community(1, "c2", vec![2]));

        let mut b = Graph::new();
        b.communities.push(community(0, "c1", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_community_label_changed() {
        let mut a = Graph::new();
        a.communities.push(community(0, "old_label", vec![1]));

        let mut b = Graph::new();
        b.communities.push(community(0, "new_label", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_community_member_added() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c", vec![1]));

        let mut b = Graph::new();
        b.communities.push(community(0, "c", vec![1, 2]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_community_member_removed() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c", vec![1, 2]));

        let mut b = Graph::new();
        b.communities.push(community(0, "c", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changes_when_community_id_changed() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c", vec![1]));

        let mut b = Graph::new();
        b.communities.push(community(1, "c", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    // ── Category 9: Concatenation-collision guards ────────────────────────────

    #[test]
    fn collision_guard_label_and_source_file_boundary() {
        // Without length prefixes: node(label="ab", file="c") and node(label="a", file="bc")
        // would feed the same bytes "abc...abc..." into the hasher. Length-prefix prevents this.
        let make = |label: &str, file: &str| {
            let mut g = Graph::new();
            g.nodes
                .push(node_full(1, label, file, Span::new(0, 1, 1, 1)));
            g
        };
        assert_ne!(
            generation_id(&make("ab", "c")),
            generation_id(&make("a", "bc")),
        );
    }

    #[test]
    fn collision_guard_relation_length_matters() {
        // Relation "abcd" vs "abc" must differ; without length prefix they might collide
        // if the confidence byte happened to equal the 'd' byte.
        let make = |rel: &str| {
            let mut g = Graph::new();
            g.edges.push(edge(1, 2, rel, Confidence::Extracted));
            g
        };
        assert_ne!(generation_id(&make("abcd")), generation_id(&make("abc")));
    }

    #[test]
    fn collision_guard_community_label_boundary() {
        // Community label "ab" vs "a" — without length prefix the label bytes bleed into the
        // subsequent member-count field.
        let make = |label: &str| {
            let mut g = Graph::new();
            g.communities.push(community(1, label, vec![]));
            g
        };
        assert_ne!(generation_id(&make("ab")), generation_id(&make("a")));
    }

    #[test]
    fn collision_guard_sections_are_independent() {
        // A graph with 1 node and 0 edges must differ from 0 nodes and 1 edge,
        // even if count bytes in isolation overlap.
        let mut a = Graph::new();
        a.nodes.push(node(1, "n"));

        let mut b = Graph::new();
        b.edges.push(edge(0, 0, "", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn collision_guard_empty_label_vs_absent_node() {
        // A graph with one node whose label is "" must differ from an empty graph:
        // the node count (1 vs 0) differs, so even zero-byte labels don't collapse.
        let mut a = Graph::new();
        a.nodes.push(node(1, ""));

        assert_ne!(generation_id(&Graph::new()), generation_id(&a));
    }

    // ── Category 10: Edge cases ───────────────────────────────────────────────

    #[test]
    fn single_node_no_edges_no_communities() {
        let mut g = Graph::new();
        g.nodes.push(node(1, "solo"));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
    }

    #[test]
    fn edges_only_no_nodes_no_communities() {
        let mut g = Graph::new();
        g.edges.push(edge(1, 2, "r", Confidence::Inferred));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
    }

    #[test]
    fn communities_only_no_nodes_no_edges() {
        let mut g = Graph::new();
        g.communities.push(community(0, "lone", vec![]));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
    }

    #[test]
    fn node_with_empty_label_differs_from_empty_graph() {
        let mut g = Graph::new();
        g.nodes.push(node(0, ""));
        assert_ne!(generation_id(&Graph::new()), generation_id(&g));
    }

    #[test]
    fn node_with_empty_source_file_differs_from_nonempty() {
        let mut a = Graph::new();
        a.nodes.push(node_full(1, "n", "", Span::new(0, 1, 1, 1)));

        let mut b = Graph::new();
        b.nodes
            .push(node_full(1, "n", "nonempty.rs", Span::new(0, 1, 1, 1)));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn edge_with_empty_relation_differs_from_nonempty() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 2, "", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "calls", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn community_with_empty_label_differs_from_nonempty() {
        let mut a = Graph::new();
        a.communities.push(community(0, "", vec![1]));

        let mut b = Graph::new();
        b.communities.push(community(0, "nonempty", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn community_with_empty_members_differs_from_one_member() {
        let mut a = Graph::new();
        a.communities.push(community(0, "c", vec![]));

        let mut b = Graph::new();
        b.communities.push(community(0, "c", vec![1]));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn unicode_label_produces_valid_id() {
        let mut a = Graph::new();
        a.nodes.push(node(1, "こんにちは"));

        let mut b = Graph::new();
        b.nodes.push(node(1, "hello"));

        let id = generation_id(&a);
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, generation_id(&b));
    }

    #[test]
    fn unicode_source_file_produces_valid_id() {
        let mut g = Graph::new();
        g.nodes
            .push(node_full(1, "n", "café/src.rs", Span::new(0, 1, 1, 1)));
        let id = generation_id(&g);
        assert_eq!(id.len(), GENERATION_LEN);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn large_graph_determinism() {
        let mut g = Graph::new();
        for i in 0..100_u32 {
            g.nodes.push(node(i, &format!("node_{i}")));
            if i > 0 {
                g.edges.push(edge(i - 1, i, "next", Confidence::Extracted));
            }
            if i % 10 == 0 {
                let start = i;
                let end = i.saturating_add(10);
                g.communities.push(community(
                    i / 10,
                    &format!("cluster_{}", i / 10),
                    (start..end).collect(),
                ));
            }
        }
        let id1 = generation_id(&g);
        let id2 = generation_id(&g);
        assert_eq!(id1, id2);
    }

    #[test]
    fn two_nodes_same_label_different_node_ids_differ() {
        // Changing the NodeId values (not just insertion order) must change the generation id.
        let mut a = Graph::new();
        a.nodes.push(node(1, "label"));
        a.nodes.push(node(2, "label"));

        let mut b = Graph::new();
        b.nodes.push(node(3, "label"));
        b.nodes.push(node(4, "label"));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn edge_self_loop_differs_from_cross_edge() {
        let mut a = Graph::new();
        a.edges.push(edge(1, 1, "self", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(1, 2, "self", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn edge_source_target_swap_differs() {
        // Directed: edge(10→20) and edge(20→10) must produce different ids.
        let mut a = Graph::new();
        a.edges.push(edge(10, 20, "r", Confidence::Extracted));

        let mut b = Graph::new();
        b.edges.push(edge(20, 10, "r", Confidence::Extracted));

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn changing_one_community_among_many_detected() {
        let mut a = Graph::new();
        a.communities.push(community(0, "first", vec![1, 2]));
        a.communities.push(community(1, "second", vec![3, 4]));

        let mut b = a.clone();
        b.communities[1].label = "CHANGED".to_owned();

        assert_ne!(generation_id(&a), generation_id(&b));
    }

    #[test]
    fn empty_graph_id_is_stable_across_instances() {
        // Two independently created empty graphs must hash identically.
        assert_eq!(generation_id(&Graph::new()), generation_id(&Graph::new()));
    }

    #[test]
    fn single_edge_with_all_confidence_variants_all_differ() {
        let ids: Vec<String> = [
            Confidence::Extracted,
            Confidence::Inferred,
            Confidence::Ambiguous,
        ]
        .iter()
        .map(|&conf| {
            let mut g = Graph::new();
            g.edges.push(edge(1, 2, "rel", conf));
            generation_id(&g)
        })
        .collect();

        // All three confidence variants produce distinct ids.
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
        assert_ne!(ids[0], ids[2]);
    }
}
