//! Deterministic 3-way `graph.json` merge driver — conflict-free git auto-merge (R4).
//!
//! `graph.json` is a generated, canonically-sorted artifact (R4), which makes a deterministic
//! 3-way merge possible.  Given the merge `base` and two branches (`ours`, `theirs`),
//! [`merge3`] produces a single merged [`Graph`] with no textual conflict markers.
//!
//! ## Semantics
//!
//! Clean-node merge identity is by **label**, while publicly redacted nodes retain stable
//! `(id, marker, provenance)` identity — the same mechanism as the 2-way
//! [`crate::merge::merge`]. For each
//! identity `L` the outcome is:
//!
//! | `L ∈ base` | `L ∈ ours` | `L ∈ theirs` | outcome |
//! |---|---|---|---|
//! | no  | yes | no  | keep (added by `ours`) |
//! | no  | no  | yes | keep (added by `theirs`) |
//! | no  | yes | yes | keep once (added by both) |
//! | yes | no  | yes | **drop** (deleted by `ours`; unchanged in `theirs`) |
//! | yes | yes | no  | **drop** (deleted by `theirs`; unchanged in `ours`) |
//! | yes | yes | yes | keep once (unchanged on both sides) |
//!
//! Edges follow the same policy keyed on
//! `(source_identity, target_identity, public_relation_identity)`. Edges whose endpoints were
//! deleted by the node-level merge are also dropped even if the edge itself survived. Communities
//! follow the same policy keyed on community label; member [`NodeId`]s are remapped to the merged
//! id space and members whose node was deleted are pruned.
//!
//! The output is passed through [`crate::dedup`] then [`Graph::sorted`] so two runs on identical
//! inputs produce byte-identical output (R4).

use std::collections::{HashMap, HashSet};

use habitat_graph_core::{
    Community, CommunityId, Edge, Graph, Manifest, Node, NodeId, PublicRelationProjector,
};

use crate::merge_identity::{
    allocate_node_id, node_id_to_identity_map, node_identity, node_identity_maps,
    node_identity_set, NodeIdentity, NodeIdentityMap,
};

type EdgeKey = (NodeIdentity, NodeIdentity, String);

/// Deterministically 3-way-merges `ours` and `theirs` against their common ancestor `base`.
///
/// Merge identity is by label for clean nodes and stable `(id, marker, provenance)` for publicly
/// redacted nodes; see the module documentation for the full deletion/addition semantics table. Node data
/// (`source_file`, `source_location`) from `ours` wins when the same identity appears in both
/// branches. The same preference applies to edge `confidence` and to community member lists.
///
/// The result is passed through [`crate::dedup`] and [`Graph::sorted`] so it is deduplicated
/// and canonically ordered (R4).
///
/// ## Guarantees
///
/// - **Deterministic** (R4): equal inputs produce byte-identical output on every run.
/// - **Conflict-free**: every difference has a clear resolution rule.
/// - **Infallible**: this function never returns an error or panics.
#[must_use]
pub fn merge3(base: &Graph, ours: &Graph, theirs: &Graph) -> Graph {
    let identity_maps = node_identity_maps(&[base, ours, theirs], Some(0));
    let base_identities = &identity_maps[0];
    let ours_identities = &identity_maps[1];
    let theirs_identities = &identity_maps[2];
    let base_node_identities = node_identity_set(&base.nodes, base_identities);
    let ours_node_identities = node_identity_set(&ours.nodes, ours_identities);
    let theirs_node_identities = node_identity_set(&theirs.nodes, theirs_identities);

    let base_id_to_identity = node_id_to_identity_map(&base.nodes, base_identities);
    let ours_id_to_identity = node_id_to_identity_map(&ours.nodes, ours_identities);
    let theirs_id_to_identity = node_id_to_identity_map(&theirs.nodes, theirs_identities);

    let ours_identity_to_node: HashMap<NodeIdentity, &Node> = ours
        .nodes
        .iter()
        .map(|node| (node_identity(node, ours_identities), node))
        .collect();

    // ── Phase 1: select winning nodes (base-aware deletion) ───────────────────
    let pre_nodes = select_winning_nodes(
        [
            (&ours.nodes, ours_identities),
            (&theirs.nodes, theirs_identities),
        ],
        &base_node_identities,
        &ours_node_identities,
        &theirs_node_identities,
        &ours_identity_to_node,
    );

    // ── Phase 2: assign NodeIds; build identity → new_id map ──────────────────
    let (merged_nodes, identity_to_new_id) = assign_node_ids(pre_nodes);

    // ── Phase 3: edge identity-key set for base-aware deletion ────────────────
    let base_edge_keys = edge_key_set(&base.edges, &base_id_to_identity);

    // ── Phase 4: merge edges ──────────────────────────────────────────────────
    let merged_edges = merge_graph_edges(
        ours,
        theirs,
        &ours_id_to_identity,
        &theirs_id_to_identity,
        &base_edge_keys,
        &identity_to_new_id,
    );

    // ── Phase 5: merge communities ────────────────────────────────────────────
    let base_comm_labels = community_label_set(&base.communities);
    let merged_communities = merge_graph_communities(
        ours,
        theirs,
        &base_comm_labels,
        &ours_id_to_identity,
        &theirs_id_to_identity,
        &identity_to_new_id,
    );

    // ── Phase 6: merge manifest ───────────────────────────────────────────────
    let manifest = merge_manifest(&ours.manifest, &theirs.manifest);

    // ── Phase 7: assemble, dedup, sort (R4) ──────────────────────────────────
    crate::dedup(Graph {
        schema: ours.schema.clone(),
        nodes: merged_nodes,
        edges: merged_edges,
        communities: merged_communities,
        manifest,
    })
    .sorted()
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

#[must_use]
fn edge_key_set(
    edges: &[Edge],
    id_to_identity: &HashMap<NodeId, NodeIdentity>,
) -> HashSet<EdgeKey> {
    let mut relation_projector = PublicRelationProjector::new();
    edges
        .iter()
        .filter_map(|e| {
            let src = id_to_identity.get(&e.source)?.clone();
            let tgt = id_to_identity.get(&e.target)?.clone();
            let relation = relation_projector.project(e.source, e.target, &e.relation);
            Some((src, tgt, relation))
        })
        .collect()
}

/// Returns a set of every community label present in `communities`.
#[must_use]
fn community_label_set(communities: &[Community]) -> HashSet<&str> {
    communities.iter().map(|c| c.label.as_str()).collect()
}

/// Selects which nodes survive the 3-way merge, applying the base-aware deletion policy.
///
/// Returned nodes retain their original `NodeId`s; call [`assign_node_ids`] next. `ours` data
/// wins when the same identity appears in both branches.
#[must_use]
fn select_winning_nodes<'a>(
    inputs: [(&'a [Node], &'a NodeIdentityMap); 2],
    base_identities: &HashSet<NodeIdentity>,
    ours_identities: &HashSet<NodeIdentity>,
    theirs_identities: &HashSet<NodeIdentity>,
    ours_identity_to_node: &HashMap<NodeIdentity, &'a Node>,
) -> Vec<(NodeIdentity, Node)> {
    let mut seen: HashSet<NodeIdentity> = HashSet::new();
    let mut result = Vec::new();

    for (nodes, identity_map) in inputs {
        for node in nodes {
            let identity = node_identity(node, identity_map);
            if !seen.insert(identity.clone()) {
                continue;
            }
            let keep = !base_identities.contains(&identity)
                || (ours_identities.contains(&identity) && theirs_identities.contains(&identity));
            if !keep {
                continue;
            }
            let winner = ours_identity_to_node
                .get(&identity)
                .copied()
                .unwrap_or(node);
            result.push((identity, winner.clone()));
        }
    }
    result
}

#[must_use]
fn assign_node_ids(nodes: Vec<(NodeIdentity, Node)>) -> (Vec<Node>, HashMap<NodeIdentity, NodeId>) {
    let mut identity_to_new_id = HashMap::with_capacity(nodes.len());
    let reserved_projected_ids: HashSet<u32> = nodes
        .iter()
        .filter_map(|(identity, _)| identity.projected_id())
        .map(NodeId::get)
        .collect();
    let mut used_ids = HashSet::with_capacity(nodes.len());
    let updated: Vec<Node> = nodes
        .into_iter()
        .map(|(identity, mut node)| {
            let new_id = allocate_node_id(&identity, &mut used_ids, &reserved_projected_ids);
            identity_to_new_id.insert(identity, new_id);
            node.id = new_id;
            node
        })
        .collect();
    (updated, identity_to_new_id)
}

/// Merges edges from `ours` and `theirs` applying base-aware deletion and node-survival filtering.
///
/// An edge is kept only when its identity-key passes the same 3-way policy as nodes AND both
/// endpoint nodes survived the node-level merge.  `ours` edges are processed first, so `ours`
/// data wins when the same key appears in both branches.
#[must_use]
fn merge_graph_edges(
    ours: &Graph,
    theirs: &Graph,
    ours_id_to_identity: &HashMap<NodeId, NodeIdentity>,
    theirs_id_to_identity: &HashMap<NodeId, NodeIdentity>,
    base_edge_keys: &HashSet<EdgeKey>,
    identity_to_new_id: &HashMap<NodeIdentity, NodeId>,
) -> Vec<Edge> {
    let ours_edge_keys = edge_key_set(&ours.edges, ours_id_to_identity);
    let theirs_edge_keys = edge_key_set(&theirs.edges, theirs_id_to_identity);

    let mut seen: HashSet<EdgeKey> = HashSet::new();
    let mut result: Vec<Edge> = Vec::new();

    for (edges, id_to_identity) in [
        (&ours.edges, ours_id_to_identity),
        (&theirs.edges, theirs_id_to_identity),
    ] {
        let mut relation_projector = PublicRelationProjector::new();
        for edge in edges {
            let Some(src_identity) = id_to_identity.get(&edge.source) else {
                continue;
            };
            let Some(tgt_identity) = id_to_identity.get(&edge.target) else {
                continue;
            };
            let key = (
                src_identity.clone(),
                tgt_identity.clone(),
                relation_projector.project(edge.source, edge.target, &edge.relation),
            );

            // Base-aware deletion: keep iff new OR kept on both sides.
            let keep = !base_edge_keys.contains(&key)
                || (ours_edge_keys.contains(&key) && theirs_edge_keys.contains(&key));
            if !keep || seen.contains(&key) {
                continue;
            }

            let Some(src_new) = identity_to_new_id.get(src_identity).copied() else {
                continue;
            };
            let Some(tgt_new) = identity_to_new_id.get(tgt_identity).copied() else {
                continue;
            };

            seen.insert(key);
            result.push(Edge {
                source: src_new,
                target: tgt_new,
                relation: edge.relation.clone(),
                confidence: edge.confidence,
            });
        }
    }
    result
}

/// Merges communities from `ours` and `theirs` applying base-aware deletion and member remapping.
///
/// Community identity is by label.  Member [`NodeId`]s are remapped to the merged id space;
/// members whose node was deleted by the merge are pruned from the member list.
#[must_use]
fn merge_graph_communities(
    ours: &Graph,
    theirs: &Graph,
    base_comm_labels: &HashSet<&str>,
    ours_id_to_identity: &HashMap<NodeId, NodeIdentity>,
    theirs_id_to_identity: &HashMap<NodeId, NodeIdentity>,
    identity_to_new_id: &HashMap<NodeIdentity, NodeId>,
) -> Vec<Community> {
    let ours_comm_labels: HashSet<&str> =
        ours.communities.iter().map(|c| c.label.as_str()).collect();
    let theirs_comm_labels: HashSet<&str> = theirs
        .communities
        .iter()
        .map(|c| c.label.as_str())
        .collect();

    let ours_comm_map: HashMap<&str, &Community> = ours
        .communities
        .iter()
        .map(|c| (c.label.as_str(), c))
        .collect();

    let mut seen: HashSet<&str> = HashSet::new();
    let mut result: Vec<Community> = Vec::new();
    let mut next_id: u32 = 0;

    for community in ours.communities.iter().chain(theirs.communities.iter()) {
        let label = community.label.as_str();
        if seen.contains(label) {
            continue;
        }
        // Base-aware deletion: keep iff new OR kept on both sides.
        let keep = !base_comm_labels.contains(label)
            || (ours_comm_labels.contains(label) && theirs_comm_labels.contains(label));
        seen.insert(label);
        if !keep {
            continue;
        }

        // Prefer ours' community data; choose the correct id-to-identity map for member remapping.
        let (comm, id_to_identity) = if let Some(&c) = ours_comm_map.get(label) {
            (c, ours_id_to_identity)
        } else {
            (community, theirs_id_to_identity)
        };

        let members: Vec<NodeId> = comm
            .members
            .iter()
            .filter_map(|&mid| {
                let member_identity = id_to_identity.get(&mid)?;
                identity_to_new_id.get(member_identity).copied()
            })
            .collect();

        result.push(Community {
            id: CommunityId::new(next_id),
            label: comm.label.clone(),
            members,
        });
        next_id = next_id.saturating_add(1);
    }
    result
}

/// Merges two manifests: concatenates inputs; takes `tool_version` from `ours`; prefers
/// `ours.generated_at`, falling back to `theirs.generated_at`.
#[must_use]
fn merge_manifest(ours: &Manifest, theirs: &Manifest) -> Manifest {
    let mut inputs = ours.inputs.clone();
    inputs.extend_from_slice(&theirs.inputs);
    Manifest {
        inputs,
        tool_version: ours.tool_version.clone(),
        generated_at: ours
            .generated_at
            .clone()
            .or_else(|| theirs.generated_at.clone()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        content_id, project_public_relation, Community, CommunityId, Confidence, Edge, Graph,
        InputRecord, Node, NodeId, Span,
    };

    use super::merge3;

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 10, 1, 2)
    }

    fn node(id: u32, label: &str) -> Node {
        node_at(id, label, "src.rs")
    }

    fn node_at(id: u32, label: &str, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span(),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str) -> Edge {
        edge_conf(src, tgt, rel, Confidence::Extracted)
    }

    fn edge_conf(src: u32, tgt: u32, rel: &str, conf: Confidence) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: conf,
        }
    }

    fn community(id: u32, label: &str, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    /// Build a graph from a list of `(id, label)` pairs.
    fn nodes_graph(pairs: &[(u32, &str)]) -> Graph {
        let mut g = Graph::new();
        for &(id, label) in pairs {
            g.nodes.push(node(id, label));
        }
        g
    }

    fn node_labels(g: &Graph) -> Vec<&str> {
        let mut v: Vec<&str> = g.nodes.iter().map(|n| n.label.as_str()).collect();
        v.sort_unstable();
        v
    }

    // ── Node merge tests ──────────────────────────────────────────────────────

    // 1. All empty → empty.
    #[test]
    fn empty_all_produces_empty() {
        let m = merge3(&Graph::new(), &Graph::new(), &Graph::new());
        assert_eq!(m.counts(), (0, 0, 0));
    }

    // 2. Empty base, ours adds a node → it appears in result.
    #[test]
    fn empty_base_ours_adds_node() {
        let ours = nodes_graph(&[(0, "Alpha")]);
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(node_labels(&m), vec!["Alpha"]);
    }

    // 3. Empty base, theirs adds a node → it appears in result.
    #[test]
    fn empty_base_theirs_adds_node() {
        let theirs = nodes_graph(&[(0, "Beta")]);
        let m = merge3(&Graph::new(), &Graph::new(), &theirs);
        assert_eq!(node_labels(&m), vec!["Beta"]);
    }

    // 4. Empty base, ours adds X, theirs adds Y (distinct) → both appear.
    #[test]
    fn empty_base_both_add_distinct_nodes() {
        let ours = nodes_graph(&[(0, "X")]);
        let theirs = nodes_graph(&[(0, "Y")]);
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(node_labels(&m), vec!["X", "Y"]);
    }

    // 5. Empty base, both add the same label → exactly one node.
    #[test]
    fn empty_base_both_add_same_label_deduped() {
        let ours = nodes_graph(&[(0, "Shared")]);
        let theirs = nodes_graph(&[(1, "Shared")]);
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].label, "Shared");
    }

    // 6. Base has X; ours drops X; theirs keeps X unchanged → X deleted.
    #[test]
    fn deletion_on_ours_is_respected() {
        let base = nodes_graph(&[(0, "X")]);
        let ours = Graph::new(); // deleted X
        let theirs = nodes_graph(&[(0, "X")]);
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.nodes.len(), 0, "X must be deleted since ours dropped it");
    }

    // 7. Base has X; theirs drops X; ours keeps X unchanged → X deleted.
    #[test]
    fn deletion_on_theirs_is_respected() {
        let base = nodes_graph(&[(0, "X")]);
        let ours = nodes_graph(&[(0, "X")]);
        let theirs = Graph::new(); // deleted X
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(
            m.nodes.len(),
            0,
            "X must be deleted since theirs dropped it"
        );
    }

    // 8. Base has X; both sides keep X → exactly one X in result.
    #[test]
    fn both_sides_keep_base_node_kept_once() {
        let base = nodes_graph(&[(0, "X")]);
        let ours = nodes_graph(&[(0, "X")]);
        let theirs = nodes_graph(&[(0, "X")]);
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].label, "X");
    }

    // 9. Both sides have the same label (new, not in base) → ours' source_file wins.
    #[test]
    fn ours_node_data_wins_over_theirs_on_shared_label() {
        let mut ours = Graph::new();
        ours.nodes.push(node_at(0, "N", "ours.rs"));
        let mut theirs = Graph::new();
        theirs.nodes.push(node_at(0, "N", "theirs.rs"));
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.nodes.len(), 1);
        assert_eq!(m.nodes[0].source_file, "ours.rs");
    }

    // 10. Base has X with ours.rs; both keep X → ours.rs wins even when theirs has different file.
    #[test]
    fn ours_data_wins_for_shared_base_node() {
        let base = nodes_graph(&[(0, "X")]);
        let mut ours = Graph::new();
        ours.nodes.push(node_at(0, "X", "ours.rs"));
        let mut theirs = Graph::new();
        theirs.nodes.push(node_at(0, "X", "theirs.rs"));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.nodes[0].source_file, "ours.rs");
    }

    // 11. Partial overlap with deletions: base {A,B}, ours {A,C}, theirs {B,D}.
    //     A deleted by theirs; B deleted by ours; C new on ours; D new on theirs → {C, D}.
    #[test]
    fn partial_overlap_with_deletion_complex() {
        let base = nodes_graph(&[(0, "A"), (1, "B")]);
        let ours = nodes_graph(&[(0, "A"), (2, "C")]);
        let theirs = nodes_graph(&[(1, "B"), (3, "D")]);
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(node_labels(&m), vec!["C", "D"]);
    }

    // 12. All base nodes deleted by ours (ours empty, theirs has all base nodes) → nothing survives.
    #[test]
    fn all_base_nodes_deleted_by_ours_nothing_survives() {
        let base = nodes_graph(&[(0, "X"), (1, "Y")]);
        let ours = Graph::new();
        let theirs = nodes_graph(&[(0, "X"), (1, "Y")]);
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.nodes.len(), 0);
    }

    // 13. All base nodes deleted by theirs (theirs empty, ours has all base nodes) → nothing survives.
    #[test]
    fn all_base_nodes_deleted_by_theirs_nothing_survives() {
        let base = nodes_graph(&[(0, "X"), (1, "Y")]);
        let ours = nodes_graph(&[(0, "X"), (1, "Y")]);
        let theirs = Graph::new();
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.nodes.len(), 0);
    }

    // 14. Fresh NodeIds are content-addressed (FO-4) — a merged graph matches a fresh assemble.
    #[test]
    fn node_ids_assigned_content_addressed() {
        let ours = nodes_graph(&[(99, "A"), (100, "B"), (101, "C")]);
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        // FO-4: each surviving node's id is content_id(label), NOT the arbitrary input id.
        for n in &m.nodes {
            assert_eq!(
                n.id.get(),
                content_id(&n.label),
                "id must be content_id(label)"
            );
        }
        assert!(
            m.nodes
                .iter()
                .all(|n| ![99, 100, 101].contains(&n.id.get())),
            "original input ids must not be retained"
        );
    }

    // 15. Output nodes are sorted by id ascending (canonical R4 order).
    #[test]
    fn output_nodes_sorted_by_id_asc() {
        let ours = nodes_graph(&[(5, "Z"), (3, "A"), (1, "M")]);
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        let ids: Vec<u32> = m.nodes.iter().map(|n| n.id.get()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    // 16. ours == base; theirs adds a new node → that node appears.
    #[test]
    fn ours_equals_base_theirs_adds_new() {
        let base = nodes_graph(&[(0, "X")]);
        let ours = nodes_graph(&[(0, "X")]);
        let mut theirs = nodes_graph(&[(0, "X"), (1, "Y")]);
        theirs.nodes.push(node(1, "Y")); // add explicitly
        let m = merge3(&base, &ours, &theirs);
        let labels = node_labels(&m);
        assert!(labels.contains(&"X"), "X kept");
        assert!(labels.contains(&"Y"), "Y added by theirs");
    }

    // 17. theirs == base; ours adds a new node → that node appears.
    #[test]
    fn theirs_equals_base_ours_adds_new() {
        let base = nodes_graph(&[(0, "X")]);
        let theirs = nodes_graph(&[(0, "X")]);
        let ours = nodes_graph(&[(0, "X"), (1, "Z")]);
        let m = merge3(&base, &ours, &theirs);
        let labels = node_labels(&m);
        assert!(labels.contains(&"X"), "X kept");
        assert!(labels.contains(&"Z"), "Z added by ours");
    }

    // 18. Multiple distinct nodes from theirs all added when base is empty.
    #[test]
    fn multiple_distinct_nodes_from_theirs_all_added() {
        let theirs = nodes_graph(&[(0, "A"), (1, "B"), (2, "C")]);
        let m = merge3(&Graph::new(), &Graph::new(), &theirs);
        assert_eq!(m.nodes.len(), 3);
    }

    // 19. Multiple distinct nodes from ours all added when base is empty.
    #[test]
    fn multiple_distinct_nodes_from_ours_all_added() {
        let ours = nodes_graph(&[(0, "P"), (1, "Q"), (2, "R")]);
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.nodes.len(), 3);
    }

    // 20. Large mixed set: base {A,B}, ours adds C,D, theirs adds E,F; base unchanged on both.
    #[test]
    fn large_node_set_mixed_additions_and_base() {
        let base = nodes_graph(&[(0, "A"), (1, "B")]);
        let ours = nodes_graph(&[(0, "A"), (1, "B"), (2, "C"), (3, "D")]);
        let theirs = nodes_graph(&[(0, "A"), (1, "B"), (4, "E"), (5, "F")]);
        let m = merge3(&base, &ours, &theirs);
        let labels = node_labels(&m);
        for l in ["A", "B", "C", "D", "E", "F"] {
            assert!(labels.contains(&l), "missing {l}");
        }
        assert_eq!(m.nodes.len(), 6);
    }

    // 21. Deletion on one side does not affect unrelated nodes.
    #[test]
    fn deletion_does_not_affect_unrelated_nodes() {
        let base = nodes_graph(&[(0, "X"), (1, "Y")]);
        let ours = nodes_graph(&[(1, "Y")]); // deleted X, kept Y
        let theirs = nodes_graph(&[(0, "X"), (1, "Y")]); // kept both
        let m = merge3(&base, &ours, &theirs);
        // X: deleted by ours, unchanged in theirs → DROP
        // Y: kept by both → KEEP
        assert_eq!(node_labels(&m), vec!["Y"]);
    }

    // ── Edge merge tests ──────────────────────────────────────────────────────

    // 22. Empty base, ours has A→B, theirs has C→D → both edges in result.
    #[test]
    fn edge_union_empty_base() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "calls"));
        let mut theirs = nodes_graph(&[(2, "C"), (3, "D")]);
        theirs.edges.push(edge(2, 3, "imports"));
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.edges.len(), 2);
    }

    // 23. Edge deletion on ours respected (base has A→B, ours drops it, theirs keeps).
    #[test]
    fn edge_deletion_on_ours_respected() {
        let mut base = nodes_graph(&[(0, "A"), (1, "B")]);
        base.edges.push(edge(0, 1, "calls"));
        let ours = nodes_graph(&[(0, "A"), (1, "B")]); // edge dropped
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "calls")); // edge kept
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(
            m.edges.len(),
            0,
            "edge must be deleted since ours dropped it"
        );
    }

    // 24. Edge deletion on theirs respected (base has A→B, theirs drops it, ours keeps).
    #[test]
    fn edge_deletion_on_theirs_respected() {
        let mut base = nodes_graph(&[(0, "A"), (1, "B")]);
        base.edges.push(edge(0, 1, "calls"));
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "calls")); // edge kept
        let theirs = nodes_graph(&[(0, "A"), (1, "B")]); // edge dropped
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(
            m.edges.len(),
            0,
            "edge must be deleted since theirs dropped it"
        );
    }

    // 25. Edge present in base and kept by both → appears once.
    #[test]
    fn edge_both_kept_appears_once() {
        let mut base = nodes_graph(&[(0, "A"), (1, "B")]);
        base.edges.push(edge(0, 1, "calls"));
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "calls"));
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "calls"));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
    }

    // 26. Same edge added by both sides (not in base) → deduped to one.
    #[test]
    fn edge_added_by_both_sides_deduped() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "calls"));
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "calls"));
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
    }

    // 27. Different relations for the same node pair → both kept.
    #[test]
    fn edge_distinct_relations_same_node_pair_both_kept() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "calls"));
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "imports"));
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.edges.len(), 2);
    }

    // 28. Edge dropped when its source node is deleted by the merge.
    #[test]
    fn edge_dropped_when_source_node_deleted() {
        // Base has A,B with edge A→B. Ours deletes A; theirs keeps both with edge.
        // After merge: A gone → edge A→B must also be dropped.
        let mut base = nodes_graph(&[(0, "A"), (1, "B")]);
        base.edges.push(edge(0, 1, "calls"));
        let ours = nodes_graph(&[(1, "B")]); // deleted A
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "calls"));
        let m = merge3(&base, &ours, &theirs);
        // A is deleted (deleted by ours, unchanged in theirs) → edge dropped
        assert_eq!(m.edges.len(), 0);
    }

    // 29. Edge dropped when its target node is deleted by the merge.
    #[test]
    fn edge_dropped_when_target_node_deleted() {
        let mut base = nodes_graph(&[(0, "A"), (1, "B")]);
        base.edges.push(edge(0, 1, "calls"));
        let ours = nodes_graph(&[(0, "A")]); // deleted B
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "calls"));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.edges.len(), 0, "edge dropped when target deleted");
    }

    // 30. Same edge on both sides → ours' confidence wins.
    #[test]
    fn edge_ours_confidence_wins_over_theirs() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges
            .push(edge_conf(0, 1, "calls", Confidence::Extracted));
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs
            .edges
            .push(edge_conf(0, 1, "calls", Confidence::Ambiguous));
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].confidence, Confidence::Extracted);
    }

    // 31. Self-loop in ours (not in base) → preserved with remapped id.
    #[test]
    fn edge_self_loop_preserved() {
        let mut ours = nodes_graph(&[(7, "Solo")]);
        ours.edges.push(edge(7, 7, "reflexive"));
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.edges.len(), 1);
        let e = &m.edges[0];
        assert_eq!(e.source, e.target, "self-loop src == tgt");
        assert_eq!(e.relation, "reflexive");
    }

    // 32. Edge remapped to merged NodeIds (ours edge).
    #[test]
    fn edge_from_ours_remapped_to_merged_ids() {
        let mut ours = nodes_graph(&[(10, "P"), (20, "Q")]);
        ours.edges.push(edge(10, 20, "link"));
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        let p_id = m.nodes.iter().find(|n| n.label == "P").map(|n| n.id);
        let q_id = m.nodes.iter().find(|n| n.label == "Q").map(|n| n.id);
        assert!(p_id.is_some() && q_id.is_some());
        assert_eq!(m.edges[0].source, p_id.unwrap());
        assert_eq!(m.edges[0].target, q_id.unwrap());
    }

    // 33. Edge from theirs remapped to merged ids.
    #[test]
    fn edge_from_theirs_remapped_to_merged_ids() {
        let mut theirs = nodes_graph(&[(100, "R"), (200, "S")]);
        theirs.edges.push(edge(100, 200, "dep"));
        let m = merge3(&Graph::new(), &Graph::new(), &theirs);
        let r_id = m.nodes.iter().find(|n| n.label == "R").map(|n| n.id);
        let s_id = m.nodes.iter().find(|n| n.label == "S").map(|n| n.id);
        assert!(r_id.is_some() && s_id.is_some());
        assert_eq!(m.edges[0].source, r_id.unwrap());
        assert_eq!(m.edges[0].target, s_id.unwrap());
    }

    // 34. Edge with dangling endpoint in ours is dropped (no panic).
    #[test]
    fn edge_dangling_endpoint_in_ours_dropped() {
        let mut ours = nodes_graph(&[(0, "A")]);
        ours.edges.push(edge(0, 99, "orphan")); // target 99 has no node
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.edges.len(), 0);
    }

    // 35. Edge with dangling endpoint in theirs is dropped (no panic).
    #[test]
    fn edge_dangling_endpoint_in_theirs_dropped() {
        let mut theirs = nodes_graph(&[(0, "B")]);
        theirs.edges.push(edge(99, 0, "orphan")); // source 99 has no node
        let m = merge3(&Graph::new(), &Graph::new(), &theirs);
        assert_eq!(m.edges.len(), 0);
    }

    // 36. Edge key uses label identity across different NodeId spaces.
    #[test]
    fn edge_label_key_matches_across_id_spaces() {
        // base: A(0)→B(1) "calls".  ours: A(100)→B(200) "calls" (same labels, diff ids).
        // theirs: A(0)→B(1) "calls".  Edge kept by both → kept once.
        let mut base = Graph::new();
        base.nodes.push(node(0, "A"));
        base.nodes.push(node(1, "B"));
        base.edges.push(edge(0, 1, "calls"));

        let mut ours = Graph::new();
        ours.nodes.push(node(100, "A"));
        ours.nodes.push(node(200, "B"));
        ours.edges.push(edge(100, 200, "calls"));

        let mut theirs = Graph::new();
        theirs.nodes.push(node(0, "A"));
        theirs.nodes.push(node(1, "B"));
        theirs.edges.push(edge(0, 1, "calls"));

        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
    }

    // 37. New edge not in base, added only by theirs → kept.
    #[test]
    fn edge_new_on_theirs_only_kept() {
        let base = nodes_graph(&[(0, "A"), (1, "B")]);
        let ours = nodes_graph(&[(0, "A"), (1, "B")]); // no edge
        let mut theirs = nodes_graph(&[(0, "A"), (1, "B")]);
        theirs.edges.push(edge(0, 1, "new-rel"));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].relation, "new-rel");
    }

    // ── Community merge tests ─────────────────────────────────────────────────

    // 38. Community added by ours (base empty) → appears in result.
    #[test]
    fn community_added_by_ours_kept() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.communities.push(community(0, "cluster-1", &[0, 1]));
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.communities.len(), 1);
        assert_eq!(m.communities[0].label, "cluster-1");
    }

    // 39. Community added by theirs (base empty) → appears in result.
    #[test]
    fn community_added_by_theirs_kept() {
        let mut theirs = nodes_graph(&[(0, "C")]);
        theirs.communities.push(community(0, "group-X", &[0]));
        let m = merge3(&Graph::new(), &Graph::new(), &theirs);
        assert_eq!(m.communities.len(), 1);
        assert_eq!(m.communities[0].label, "group-X");
    }

    // 40. Community deletion on ours respected (base has comm, ours drops it, theirs keeps).
    #[test]
    fn community_deletion_on_ours_respected() {
        let mut base = Graph::new();
        base.communities.push(community(0, "c1", &[]));
        let ours = Graph::new(); // dropped c1
        let mut theirs = Graph::new();
        theirs.communities.push(community(0, "c1", &[]));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.communities.len(), 0);
    }

    // 41. Community deletion on theirs respected (base has comm, theirs drops it, ours keeps).
    #[test]
    fn community_deletion_on_theirs_respected() {
        let mut base = Graph::new();
        base.communities.push(community(0, "c2", &[]));
        let mut ours = Graph::new();
        ours.communities.push(community(0, "c2", &[]));
        let theirs = Graph::new(); // dropped c2
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.communities.len(), 0);
    }

    // 42. Community kept by both sides → appears exactly once.
    #[test]
    fn community_both_sides_keep_base_comm_kept_once() {
        let mut base = Graph::new();
        base.communities.push(community(0, "shared", &[]));
        let mut ours = Graph::new();
        ours.communities.push(community(0, "shared", &[]));
        let mut theirs = Graph::new();
        theirs.communities.push(community(0, "shared", &[]));
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.communities.len(), 1);
    }

    // 43. Community member NodeIds are remapped to the merged id space.
    #[test]
    fn community_member_ids_remapped() {
        let mut ours = nodes_graph(&[(50, "Node-A"), (60, "Node-B")]);
        ours.communities.push(community(0, "grp", &[50, 60]));
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        let a_id = m
            .nodes
            .iter()
            .find(|n| n.label == "Node-A")
            .map(|n| n.id)
            .unwrap();
        let b_id = m
            .nodes
            .iter()
            .find(|n| n.label == "Node-B")
            .map(|n| n.id)
            .unwrap();
        let members = &m.communities[0].members;
        let mut sorted_members = members.clone();
        sorted_members.sort_unstable();
        assert!(sorted_members.contains(&a_id));
        assert!(sorted_members.contains(&b_id));
    }

    // 44. Community member whose node no longer exists is pruned from the member list.
    //
    // `ours` declares community "c" with members [NodeId(0)=A, NodeId(1)=B], but its node
    // list only contains B (NodeId 1). When remapping, A's old id (0) has no entry in
    // `ours_id_to_label`, so it is silently pruned. Only B's new merged id survives.
    #[test]
    fn community_deleted_member_pruned() {
        let mut ours = Graph::new();
        ours.nodes.push(node(1, "B")); // only B; A is absent
                                       // Community references both A(0) and B(1), but A's id is dangling in ours.
        ours.communities.push(community(0, "c", &[0, 1]));
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.communities.len(), 1, "community 'c' must survive");
        // Only B is in the merged node list → only B's new id in members.
        let b_id = m
            .nodes
            .iter()
            .find(|n| n.label == "B")
            .map(|n| n.id)
            .unwrap();
        assert_eq!(
            m.communities[0].members,
            vec![b_id],
            "dangling member A must be pruned"
        );
    }

    // 45. Both sides add the same community (not in base) → deduped to one.
    #[test]
    fn community_both_add_same_comm_deduped() {
        let mut ours = Graph::new();
        ours.communities.push(community(0, "new-c", &[]));
        let mut theirs = Graph::new();
        theirs.communities.push(community(1, "new-c", &[])); // same label, different id
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.communities.len(), 1);
        assert_eq!(m.communities[0].label, "new-c");
    }

    // ── Manifest tests ────────────────────────────────────────────────────────

    // 46. Inputs from both sides are concatenated.
    #[test]
    fn manifest_inputs_from_both_concatenated() {
        let mut ours = Graph::new();
        ours.manifest.inputs.push(InputRecord {
            path: "a.rs".to_owned(),
            content_hash: "h1".to_owned(),
        });
        let mut theirs = Graph::new();
        theirs.manifest.inputs.push(InputRecord {
            path: "b.rs".to_owned(),
            content_hash: "h2".to_owned(),
        });
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.manifest.inputs.len(), 2);
        let paths: Vec<&str> = m.manifest.inputs.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"a.rs"));
        assert!(paths.contains(&"b.rs"));
    }

    // 47. tool_version from ours wins.
    #[test]
    fn manifest_tool_version_from_ours() {
        let mut ours = Graph::new();
        ours.manifest.tool_version = "ours-1.0".to_owned();
        let mut theirs = Graph::new();
        theirs.manifest.tool_version = "theirs-9.9".to_owned();
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.manifest.tool_version, "ours-1.0");
    }

    // 48. generated_at prefers ours when both are set.
    #[test]
    fn manifest_generated_at_prefers_ours() {
        let mut ours = Graph::new();
        ours.manifest.generated_at = Some("ours-ts".to_owned());
        let mut theirs = Graph::new();
        theirs.manifest.generated_at = Some("theirs-ts".to_owned());
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.manifest.generated_at.as_deref(), Some("ours-ts"));
    }

    // 49. generated_at falls back to theirs when ours is None.
    #[test]
    fn manifest_generated_at_falls_back_to_theirs() {
        let ours = Graph::new(); // generated_at == None
        let mut theirs = Graph::new();
        theirs.manifest.generated_at = Some("theirs-ts".to_owned());
        let m = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(m.manifest.generated_at.as_deref(), Some("theirs-ts"));
    }

    // ── Invariant tests ───────────────────────────────────────────────────────

    // 50. Two runs on identical inputs produce identical output (determinism, R4).
    #[test]
    fn determinism_two_runs_identical() {
        let base = nodes_graph(&[(0, "A"), (1, "B")]);
        let mut ours = nodes_graph(&[(0, "A"), (2, "C")]);
        ours.edges.push(edge(0, 2, "calls"));
        let mut theirs = nodes_graph(&[(1, "B"), (3, "D")]);
        theirs.edges.push(edge(1, 3, "imports"));
        let m1 = merge3(&base, &ours, &theirs);
        let m2 = merge3(&base, &ours, &theirs);
        assert_eq!(m1, m2);
    }

    // 51. merge3(base, merged, merged) ≈ merged (idempotency under label-stable state).
    #[test]
    fn idempotent_merge3_with_itself() {
        let base = nodes_graph(&[(0, "A")]);
        let mut merged = nodes_graph(&[(0, "A"), (1, "B")]);
        merged.edges.push(edge(0, 1, "calls"));
        let m = merge3(&base, &merged, &merged);
        // All labels in `merged` are also in `base` or are new-and-in-both-sides.
        // A: in base, in ours(merged), in theirs(merged) → kept
        // B: not in base, in ours(merged), in theirs(merged) → kept
        // edge A→B: not in base, in ours, in theirs → kept
        assert!(m.nodes.iter().any(|n| n.label == "A"));
        assert!(m.nodes.iter().any(|n| n.label == "B"));
        assert_eq!(m.edges.len(), 1);
    }

    // 52. Schema tag is preserved from ours.
    #[test]
    fn schema_tag_preserved_from_ours() {
        use habitat_graph_core::SCHEMA_VERSION;
        let m = merge3(&Graph::new(), &Graph::new(), &Graph::new());
        assert_eq!(m.schema, SCHEMA_VERSION);
    }

    // 53. Output is always in canonical sorted order.
    #[test]
    fn output_canonically_sorted() {
        let mut ours = nodes_graph(&[(5, "Z"), (1, "A")]);
        ours.edges.push(edge(5, 1, "dep"));
        let theirs = nodes_graph(&[(3, "M")]);
        let m = merge3(&Graph::new(), &ours, &theirs);
        // Nodes must be sorted by id.
        let ids: Vec<u32> = m.nodes.iter().map(|n| n.id.get()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "nodes must be id-sorted");
        // Edges must be sorted by (source, target, relation).
        if m.edges.len() > 1 {
            for w in m.edges.windows(2) {
                assert!(
                    (w[0].source, w[0].target, w[0].relation.as_str())
                        <= (w[1].source, w[1].target, w[1].relation.as_str())
                );
            }
        }
    }

    // 54. base == ours == theirs → exactly one copy of each node/edge.
    #[test]
    fn base_equals_ours_equals_theirs_one_copy_each() {
        let mut g = Graph::new();
        g.nodes.push(node(0, "X"));
        g.nodes.push(node(1, "Y"));
        g.edges.push(edge(0, 1, "calls"));
        let m = merge3(&g, &g, &g);
        assert_eq!(m.nodes.len(), 2);
        assert_eq!(m.edges.len(), 1);
    }

    // 55. Deletion semantics: ours drops a node even though theirs explicitly has it unchanged.
    //     This documents the "respect deletion" contract explicitly.
    #[test]
    fn deletion_takes_priority_over_theirs_retention() {
        let base = nodes_graph(&[(0, "Candidate")]);
        let ours = Graph::new(); // deliberately dropped Candidate
        let theirs = nodes_graph(&[(0, "Candidate")]); // still has it
        let m = merge3(&base, &ours, &theirs);
        assert!(
            m.nodes.is_empty(),
            "deletion by ours must be respected even though theirs keeps Candidate"
        );
    }

    // 56. Edge added only by ours (not in base) → kept.
    #[test]
    fn edge_new_on_ours_only_kept() {
        let base = nodes_graph(&[(0, "A"), (1, "B")]);
        let mut ours = nodes_graph(&[(0, "A"), (1, "B")]);
        ours.edges.push(edge(0, 1, "new-calls"));
        let theirs = nodes_graph(&[(0, "A"), (1, "B")]); // no edge
        let m = merge3(&base, &ours, &theirs);
        assert_eq!(m.edges.len(), 1);
        assert_eq!(m.edges[0].relation, "new-calls");
    }

    // 57. All-disjoint: ours and theirs have completely different labels (no base) → full union.
    #[test]
    fn disjoint_ours_theirs_full_union() {
        let ours = nodes_graph(&[(0, "P"), (1, "Q")]);
        let theirs = nodes_graph(&[(0, "R"), (1, "S")]);
        let m = merge3(&Graph::new(), &ours, &theirs);
        let labels = node_labels(&m);
        for l in ["P", "Q", "R", "S"] {
            assert!(labels.contains(&l));
        }
        assert_eq!(m.nodes.len(), 4);
    }

    // 58. Edges output sorted by (source, target, relation) for byte-stable R4 diff.
    #[test]
    fn edges_output_sorted_by_tuple() {
        let mut ours = nodes_graph(&[(0, "A"), (1, "B"), (2, "C")]);
        ours.edges.push(edge(2, 0, "z-rel")); // would sort last
        ours.edges.push(edge(0, 1, "a-rel")); // would sort first
        let m = merge3(&Graph::new(), &ours, &Graph::new());
        assert_eq!(m.edges.len(), 2);
        // After sorted(), (src, tgt, rel) of edges[0] <= edges[1]
        let e0 = &m.edges[0];
        let e1 = &m.edges[1];
        assert!(
            (e0.source, e0.target, e0.relation.as_str())
                <= (e1.source, e1.target, e1.relation.as_str())
        );
    }

    // 59. Node count correct with multiple additions and deletions simultaneously.
    #[test]
    fn node_count_correct_complex_scenario() {
        // base: {A, B, C}
        // ours: {A, C, D}  (dropped B, kept A+C, added D)
        // theirs: {A, B, E}  (dropped C, kept A+B, added E)
        // Expected:
        //   A: base+ours+theirs → KEEP
        //   B: base+theirs, not ours → DROP (deleted by ours)
        //   C: base+ours, not theirs → DROP (deleted by theirs)
        //   D: not base, ours only → KEEP
        //   E: not base, theirs only → KEEP
        // Result: {A, D, E} = 3 nodes
        let base = nodes_graph(&[(0, "A"), (1, "B"), (2, "C")]);
        let ours = nodes_graph(&[(0, "A"), (2, "C"), (3, "D")]);
        let theirs = nodes_graph(&[(0, "A"), (1, "B"), (4, "E")]);
        let m = merge3(&base, &ours, &theirs);
        let labels = node_labels(&m);
        assert_eq!(labels, vec!["A", "D", "E"]);
    }

    // 60. Empty ours + base-aware semantics: theirs-only nodes that are in base get deleted.
    #[test]
    fn theirs_base_nodes_deleted_when_ours_empty() {
        // base: {X}, ours: {} (deleted X), theirs: {X, Y}
        // X: in base, not in ours → DELETE
        // Y: not in base, in theirs only → KEEP
        let base = nodes_graph(&[(0, "X")]);
        let ours = Graph::new();
        let theirs = nodes_graph(&[(0, "X"), (1, "Y")]);
        let m = merge3(&base, &ours, &theirs);
        let labels = node_labels(&m);
        assert_eq!(labels, vec!["Y"]);
    }

    #[test]
    fn redacted_public_merge_preserves_distinct_ids_and_topology() {
        let mut base = Graph::new();
        base.nodes.push(node(10, "[REDACTED:api_key]"));
        base.nodes.push(node(20, "[REDACTED:api_key]"));
        base.nodes.push(node(30, "Safe"));
        base.edges.push(edge(10, 30, "calls"));
        base.edges.push(edge(20, 30, "calls"));
        let ours = base.clone();
        let theirs = base.clone();

        let merged = merge3(&base, &ours, &theirs);
        let safe_id = merged
            .nodes
            .iter()
            .find(|node| node.label == "Safe")
            .unwrap()
            .id;
        assert_eq!(merged.nodes.len(), 3);
        assert!(merged.nodes.iter().any(|node| node.id.get() == 10));
        assert!(merged.nodes.iter().any(|node| node.id.get() == 20));
        assert_eq!(merged.edges.len(), 2);
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source.get() == 10 && edge.target == safe_id));
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source.get() == 20 && edge.target == safe_id));
    }

    #[test]
    fn redacted_public_merge_uses_label_identity_for_clean_nodes() {
        let mut base = nodes_graph(&[(90, "[REDACTED:api_key]"), (5, "Shared")]);
        let mut ours = nodes_graph(&[(90, "[REDACTED:api_key]"), (6, "Shared"), (10, "Alpha")]);
        let mut theirs = nodes_graph(&[(90, "[REDACTED:api_key]"), (7, "Shared"), (10, "Beta")]);
        base.edges.push(edge(90, 5, "existing"));
        ours.edges.push(edge(6, 10, "ours"));
        theirs.edges.push(edge(7, 10, "theirs"));

        let merged = merge3(&base, &ours, &theirs);
        assert_eq!(
            merged
                .nodes
                .iter()
                .filter(|node| node.label == "Shared")
                .count(),
            1
        );
        assert!(merged.nodes.iter().any(|node| node.label == "Alpha"));
        assert!(merged.nodes.iter().any(|node| node.label == "Beta"));

        let shared = merged
            .nodes
            .iter()
            .find(|node| node.label == "Shared")
            .unwrap()
            .id;
        let alpha = merged
            .nodes
            .iter()
            .find(|node| node.label == "Alpha")
            .unwrap()
            .id;
        let beta = merged
            .nodes
            .iter()
            .find(|node| node.label == "Beta")
            .unwrap()
            .id;
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source == shared && edge.target == alpha));
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source == shared && edge.target == beta));
    }

    #[test]
    fn redacted_id_collision_keeps_clean_node_distinct() {
        let collision_id = content_id("Safe");
        let ours = nodes_graph(&[(collision_id, "[REDACTED:api_key]")]);
        let theirs = nodes_graph(&[(collision_id, "Safe")]);

        let merged = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(merged.nodes.len(), 2);
        let marker = merged
            .nodes
            .iter()
            .find(|node| node.label == "[REDACTED:api_key]")
            .unwrap()
            .id;
        let safe = merged
            .nodes
            .iter()
            .find(|node| node.label == "Safe")
            .unwrap()
            .id;
        assert_eq!(marker.get(), collision_id);
        assert_ne!(marker, safe);
    }

    #[test]
    fn collision_probed_secret_matches_public_node_in_three_way_merge() {
        let probed_id = content_id("api_key_alpha").wrapping_add(1);
        let ours = nodes_graph(&[(probed_id, "api_key_alpha")]);
        let theirs = nodes_graph(&[(probed_id, "[REDACTED:api_key]")]);

        let merged = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(merged.nodes.len(), 1);
        assert_eq!(merged.nodes[0].id.get(), probed_id);
        assert_eq!(merged.nodes[0].label, "api_key_alpha");
    }

    #[test]
    fn raw_and_projected_edges_share_three_way_identity() {
        let mut ours = nodes_graph(&[(1, "A"), (2, "B")]);
        ours.edges.push(edge(1, 2, "api_key=alpha"));
        let mut theirs = nodes_graph(&[(10, "A"), (20, "B")]);
        theirs
            .edges
            .push(edge(10, 20, &project_public_relation("api_key=alpha")));

        let merged = merge3(&Graph::new(), &ours, &theirs);
        assert_eq!(merged.edges.len(), 1);
        assert_eq!(merged.edges[0].relation, "api_key=alpha");
    }

    #[test]
    fn different_redaction_markers_sharing_an_input_id_survive_merge3() {
        let mut ours = nodes_graph(&[(7, "[REDACTED:api_key]"), (8, "ATarget")]);
        ours.edges.push(edge(7, 8, "a-edge"));
        let mut theirs = nodes_graph(&[(7, "[REDACTED:bearer_token]"), (8, "BTarget")]);
        theirs.edges.push(edge(7, 8, "b-edge"));

        let merged = merge3(&Graph::new(), &ours, &theirs);
        let api = merged
            .nodes
            .iter()
            .find(|node| node.label == "[REDACTED:api_key]")
            .unwrap();
        let bearer = merged
            .nodes
            .iter()
            .find(|node| node.label == "[REDACTED:bearer_token]")
            .unwrap();
        let a_target = merged
            .nodes
            .iter()
            .find(|node| node.label == "ATarget")
            .unwrap();
        let b_target = merged
            .nodes
            .iter()
            .find(|node| node.label == "BTarget")
            .unwrap();

        assert_eq!(merged.nodes.len(), 4);
        assert_ne!(api.id, bearer.id);
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source == api.id && edge.target == a_target.id));
        assert!(merged
            .edges
            .iter()
            .any(|edge| edge.source == bearer.id && edge.target == b_target.id));
    }

    #[test]
    fn independently_added_identical_markers_keep_their_topology_in_merge3() {
        let mut ours = nodes_graph(&[(7, "[REDACTED:api_key]"), (8, "ATarget")]);
        ours.edges.push(edge(7, 8, "a-edge"));
        ours.communities.push(community(10, "a-community", &[7]));
        let mut theirs = nodes_graph(&[(7, "[REDACTED:api_key]"), (8, "BTarget")]);
        theirs.edges.push(edge(7, 8, "b-edge"));
        theirs.communities.push(community(11, "b-community", &[7]));

        let merged = merge3(&Graph::new(), &ours, &theirs);
        let marker_ids: Vec<NodeId> = merged
            .nodes
            .iter()
            .filter(|node| node.label == "[REDACTED:api_key]")
            .map(|node| node.id)
            .collect();
        let a_source = merged
            .edges
            .iter()
            .find(|edge| edge.relation == "a-edge")
            .unwrap()
            .source;
        let b_source = merged
            .edges
            .iter()
            .find(|edge| edge.relation == "b-edge")
            .unwrap()
            .source;

        assert_eq!(marker_ids.len(), 2);
        assert_ne!(a_source, b_source);
        assert!(marker_ids.contains(&a_source));
        assert!(marker_ids.contains(&b_source));
        assert_eq!(
            merged
                .communities
                .iter()
                .find(|community| community.label == "a-community")
                .unwrap()
                .members,
            vec![a_source]
        );
        assert_eq!(
            merged
                .communities
                .iter()
                .find(|community| community.label == "b-community")
                .unwrap()
                .members,
            vec![b_source]
        );
    }
}
