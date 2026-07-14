//! Provenance-aware identities shared by incremental and three-way graph merges.
//!
//! Clean nodes keep label identity. A node whose public label is a lossy redaction instead keeps
//! its original [`NodeId`] plus input provenance, preventing unrelated secrets that share one
//! marker from collapsing into a single node. Relations follow the same split: exact raw text is
//! compared directly, while public markers receive endpoint-local ordinals and input provenance.
//! Raw and projected identities therefore remain distinct without deriving a public digest from
//! secret text.

use std::collections::{HashMap, HashSet};

use habitat_graph_core::{
    content_id as label_content_id, is_canonical_redaction_marker, project_public_relation,
    redact_public_text, Edge, Graph, Node, NodeId, PublicRelationProjector,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NodeIdentity {
    Projected {
        assigned_id: NodeId,
        content_id: NodeId,
        provenance: usize,
    },
    Label(String),
}

impl NodeIdentity {
    pub(crate) const fn projected_id(&self) -> Option<NodeId> {
        match self {
            Self::Projected { assigned_id, .. } => Some(*assigned_id),
            Self::Label(_) => None,
        }
    }

    pub(crate) fn content_id(&self) -> NodeId {
        match self {
            Self::Projected { content_id, .. } => *content_id,
            Self::Label(label) => NodeId::new(label_content_id(label)),
        }
    }
}

pub(crate) type NodeIdentityMap = HashMap<NodeId, NodeIdentity>;

#[derive(Debug)]
struct ProjectionCandidate {
    graph_index: usize,
    assigned_id: NodeId,
    content_id: NodeId,
    marker: bool,
    raw_label: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RelationIdentity {
    Exact(String),
    Projected(String, usize),
}

fn label_identity_maps(graphs: &[&Graph]) -> Vec<NodeIdentityMap> {
    graphs
        .iter()
        .map(|graph| {
            graph
                .nodes
                .iter()
                .map(|node| (node.id, NodeIdentity::Label(node.label.clone())))
                .collect()
        })
        .collect()
}

fn projection_candidates(graphs: &[&Graph]) -> HashMap<NodeId, Vec<ProjectionCandidate>> {
    let mut candidates: HashMap<NodeId, Vec<ProjectionCandidate>> = HashMap::new();
    for (graph_index, graph) in graphs.iter().enumerate() {
        for node in &graph.nodes {
            let (marker, raw_label) = if is_canonical_redaction_marker(&node.label) {
                (true, None)
            } else {
                let projected = redact_public_text(&node.label);
                if projected.as_ref() == node.label {
                    continue;
                }
                (false, Some(node.label.clone()))
            };
            let content_id = graph.node_content_id(node.id);
            candidates
                .entry(content_id)
                .or_default()
                .push(ProjectionCandidate {
                    graph_index,
                    assigned_id: node.id,
                    content_id,
                    marker,
                    raw_label,
                });
        }
    }
    candidates
}

pub(crate) fn node_identity_maps(
    graphs: &[&Graph],
    lineage_root: Option<usize>,
) -> Vec<NodeIdentityMap> {
    let mut maps = label_identity_maps(graphs);
    let candidates = projection_candidates(graphs);

    for group in candidates.values() {
        if !group.iter().any(|candidate| candidate.marker) {
            continue;
        }
        let lineage_anchor = lineage_root
            .filter(|root| group.iter().any(|candidate| candidate.graph_index == *root));
        let mut graph_indices = HashSet::with_capacity(group.len());
        let unique_by_graph = group
            .iter()
            .all(|candidate| graph_indices.insert(candidate.graph_index));

        if let Some(anchor) = lineage_anchor.filter(|_| unique_by_graph) {
            let Some(anchor_assigned_id) = group
                .iter()
                .find(|candidate| candidate.graph_index == anchor)
                .map(|candidate| candidate.assigned_id)
            else {
                continue;
            };
            let anchor_raw_label = group
                .iter()
                .find(|candidate| candidate.graph_index == anchor)
                .and_then(|candidate| candidate.raw_label.clone());
            for candidate in group {
                if candidate.marker || candidate.raw_label == anchor_raw_label {
                    maps[candidate.graph_index].insert(
                        candidate.assigned_id,
                        NodeIdentity::Projected {
                            assigned_id: anchor_assigned_id,
                            content_id: candidate.content_id,
                            provenance: anchor,
                        },
                    );
                }
            }
        } else if let Some(anchor) = lineage_anchor {
            let anchor_candidates: HashMap<_, _> = group
                .iter()
                .filter(|candidate| candidate.graph_index == anchor)
                .map(|candidate| (candidate.assigned_id, candidate))
                .collect();
            for candidate in group {
                if let Some(anchor_candidate) = anchor_candidates
                    .get(&candidate.assigned_id)
                    .filter(|anchor_candidate| {
                        candidate.marker || candidate.raw_label == anchor_candidate.raw_label
                    })
                {
                    maps[candidate.graph_index].insert(
                        candidate.assigned_id,
                        NodeIdentity::Projected {
                            assigned_id: anchor_candidate.assigned_id,
                            content_id: candidate.content_id,
                            provenance: anchor,
                        },
                    );
                } else if candidate.marker {
                    maps[candidate.graph_index].insert(
                        candidate.assigned_id,
                        NodeIdentity::Projected {
                            assigned_id: candidate.assigned_id,
                            content_id: candidate.content_id,
                            provenance: candidate.graph_index,
                        },
                    );
                }
            }
        } else {
            for candidate in group.iter().filter(|candidate| candidate.marker) {
                maps[candidate.graph_index].insert(
                    candidate.assigned_id,
                    NodeIdentity::Projected {
                        assigned_id: candidate.assigned_id,
                        content_id: candidate.content_id,
                        provenance: candidate.graph_index,
                    },
                );
            }
        }
    }

    if let Some(root) = lineage_root.filter(|root| *root < graphs.len()) {
        bridge_legacy_projection_lineage(graphs, root, &candidates, &mut maps);
    }

    maps
}

fn projected_slot_counts(
    graph_count: usize,
    candidates: &HashMap<NodeId, Vec<ProjectionCandidate>>,
) -> Vec<HashMap<NodeId, usize>> {
    let mut counts: Vec<HashMap<NodeId, usize>> =
        (0..graph_count).map(|_| HashMap::new()).collect();
    for candidate in candidates.values().flatten() {
        *counts[candidate.graph_index]
            .entry(candidate.assigned_id)
            .or_default() += 1;
    }
    counts
}

fn legacy_bridge_anchor_is_unambiguous(
    anchor: &ProjectionCandidate,
    side_candidates: &[&ProjectionCandidate],
    slot_counts: &[HashMap<NodeId, usize>],
) -> bool {
    side_candidates.iter().all(|candidate| {
        slot_counts[candidate.graph_index]
            .get(&anchor.assigned_id)
            .copied()
            .unwrap_or_default()
            == usize::from(candidate.assigned_id == anchor.assigned_id)
    })
}

fn bridge_legacy_projection_lineage(
    graphs: &[&Graph],
    root: usize,
    candidates: &HashMap<NodeId, Vec<ProjectionCandidate>>,
    maps: &mut [NodeIdentityMap],
) {
    let probe_positions: Vec<_> = graphs
        .iter()
        .map(|graph| projection_probe_positions(graph))
        .collect();
    let mut root_anchors: HashMap<usize, Vec<(usize, &ProjectionCandidate)>> = HashMap::new();
    for candidate in candidates.values().flatten().filter(|candidate| {
        candidate.graph_index == root
            && candidate.marker
            && !graphs[root]
                .node_content_ids
                .contains_key(&candidate.assigned_id)
    }) {
        if let Some(&(group, position)) = probe_positions[root].get(&candidate.assigned_id) {
            root_anchors
                .entry(group)
                .or_default()
                .push((position, candidate));
        }
    }
    for anchors in root_anchors.values_mut() {
        anchors.sort_unstable_by_key(|(position, _)| *position);
    }
    let slot_counts = projected_slot_counts(graphs.len(), candidates);
    let mut bridges = Vec::new();

    for (&content_id, group) in candidates {
        if group.iter().any(|candidate| candidate.graph_index == root) {
            continue;
        }

        let mut side_candidates = Vec::with_capacity(graphs.len().saturating_sub(1));
        let mut valid = true;
        for graph_index in 0..graphs.len() {
            if graph_index == root {
                continue;
            }
            let mut matches = group
                .iter()
                .filter(|candidate| candidate.graph_index == graph_index);
            let Some(candidate) = matches.next() else {
                valid = false;
                break;
            };
            if matches.next().is_some()
                || !candidate.marker
                || candidate.assigned_id == content_id
                || graphs[graph_index]
                    .node_content_ids
                    .get(&candidate.assigned_id)
                    != Some(&content_id)
                || !follows_occupied_probe_chain(
                    &probe_positions[graph_index],
                    content_id,
                    candidate.assigned_id,
                )
            {
                valid = false;
                break;
            }
            side_candidates.push(candidate);
        }
        if !valid {
            continue;
        }

        let Some(&(root_group, content_position)) = probe_positions[root].get(&content_id) else {
            continue;
        };
        let Some(anchors) = root_anchors.get(&root_group) else {
            continue;
        };
        let first = anchors.partition_point(|(position, _)| *position <= content_position);
        if anchors.len().saturating_sub(first) != 1 {
            continue;
        }
        let anchor = anchors[first].1;
        if !legacy_bridge_anchor_is_unambiguous(anchor, &side_candidates, &slot_counts) {
            continue;
        }
        bridges.push((content_id, anchor, side_candidates));
    }

    let mut anchor_counts = HashMap::new();
    for (_, anchor, _) in &bridges {
        *anchor_counts.entry(anchor.assigned_id).or_insert(0_usize) += 1;
    }
    for (content_id, anchor, side_candidates) in bridges {
        if anchor_counts.get(&anchor.assigned_id) != Some(&1) {
            continue;
        }
        let identity = NodeIdentity::Projected {
            assigned_id: anchor.assigned_id,
            content_id,
            provenance: root,
        };
        maps[root].insert(anchor.assigned_id, identity.clone());
        for candidate in side_candidates {
            maps[candidate.graph_index].insert(candidate.assigned_id, identity.clone());
        }
    }
}

fn follows_occupied_probe_chain(
    positions: &HashMap<NodeId, (usize, usize)>,
    content_id: NodeId,
    assigned_id: NodeId,
) -> bool {
    match (positions.get(&content_id), positions.get(&assigned_id)) {
        (Some((content_group, content_position)), Some((assigned_group, assigned_position))) => {
            content_group == assigned_group && assigned_position > content_position
        }
        _ => false,
    }
}

fn projection_probe_positions(graph: &Graph) -> HashMap<NodeId, (usize, usize)> {
    occupied_collision_groups(graph)
        .into_iter()
        .enumerate()
        .flat_map(|(group, ids)| {
            ids.into_iter()
                .enumerate()
                .map(move |(position, id)| (id, (group, position)))
        })
        .collect()
}

pub(crate) fn occupied_collision_groups(graph: &Graph) -> Vec<Vec<NodeId>> {
    let mut ids: Vec<_> = graph.nodes.iter().map(|node| node.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut groups: Vec<Vec<NodeId>> = Vec::new();
    for id in ids {
        if groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|last| last.get() != u32::MAX && last.get() + 1 == id.get())
        {
            if let Some(group) = groups.last_mut() {
                group.push(id);
            }
        } else {
            groups.push(vec![id]);
        }
    }
    if groups.len() > 1
        && groups
            .first()
            .and_then(|group| group.first())
            .is_some_and(|id| id.get() == 0)
        && groups
            .last()
            .and_then(|group| group.last())
            .is_some_and(|id| id.get() == u32::MAX)
    {
        let first = groups.remove(0);
        if let Some(last) = groups.last_mut() {
            last.extend(first);
        }
    }
    groups
}

pub(crate) fn relation_identities(
    edges: &[Edge],
    projected_provenance: usize,
) -> Vec<RelationIdentity> {
    let mut projector = PublicRelationProjector::new();
    edges
        .iter()
        .map(|edge| {
            if is_projected_relation(&edge.relation) {
                RelationIdentity::Projected(
                    projector.project(edge.source, edge.target, &edge.relation),
                    projected_provenance,
                )
            } else {
                RelationIdentity::Exact(edge.relation.clone())
            }
        })
        .collect()
}

pub(crate) fn is_lossy_relation(relation: &str) -> bool {
    is_canonical_redaction_marker(&project_public_relation(relation))
}

fn is_projected_relation(relation: &str) -> bool {
    let marker = project_public_relation(relation);
    is_canonical_redaction_marker(&marker) && relation.starts_with(&marker)
}

pub(crate) fn node_identity(node: &Node, identities: &NodeIdentityMap) -> NodeIdentity {
    identities
        .get(&node.id)
        .cloned()
        .unwrap_or_else(|| NodeIdentity::Label(node.label.clone()))
}

pub(crate) fn allocate_node_id(
    identity: &NodeIdentity,
    used_ids: &mut HashSet<u32>,
    reserved_projected_ids: &HashSet<u32>,
) -> NodeId {
    let (preferred, projected) = match identity {
        NodeIdentity::Projected { assigned_id, .. } => (assigned_id.get(), true),
        NodeIdentity::Label(label) => (label_content_id(label), false),
    };
    let mut raw = preferred;
    loop {
        let reserved_for_other =
            reserved_projected_ids.contains(&raw) && !(projected && raw == preferred);
        if !reserved_for_other && used_ids.insert(raw) {
            return NodeId::new(raw);
        }
        raw = raw.wrapping_add(1);
    }
}

pub(crate) fn node_identity_set(
    nodes: &[Node],
    identities: &NodeIdentityMap,
) -> HashSet<NodeIdentity> {
    nodes
        .iter()
        .map(|node| node_identity(node, identities))
        .collect()
}

pub(crate) fn node_id_to_identity_map(
    nodes: &[Node],
    identities: &NodeIdentityMap,
) -> HashMap<NodeId, NodeIdentity> {
    nodes
        .iter()
        .map(|node| (node.id, node_identity(node, identities)))
        .collect()
}
