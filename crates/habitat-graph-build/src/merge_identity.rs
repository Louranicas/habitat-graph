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
    content_id, is_canonical_redaction_marker, project_public_relation, redact_public_text, Edge,
    Graph, Node, NodeId, PublicRelationProjector,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NodeIdentity {
    Projected(NodeId, usize),
    Label(String),
}

impl NodeIdentity {
    pub(crate) const fn projected_id(&self) -> Option<NodeId> {
        match self {
            Self::Projected(id, _) => Some(*id),
            Self::Label(_) => None,
        }
    }
}

pub(crate) type NodeIdentityMap = HashMap<NodeId, NodeIdentity>;

#[derive(Debug)]
struct ProjectionCandidate {
    graph_index: usize,
    marker: bool,
    raw_label: Option<String>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RelationIdentity {
    Exact(String),
    Projected(String, usize),
}

pub(crate) fn node_identity_maps(
    graphs: &[&Graph],
    lineage_root: Option<usize>,
) -> Vec<NodeIdentityMap> {
    let mut maps: Vec<NodeIdentityMap> = graphs
        .iter()
        .map(|graph| {
            graph
                .nodes
                .iter()
                .map(|node| (node.id, NodeIdentity::Label(node.label.clone())))
                .collect()
        })
        .collect();
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
            candidates
                .entry(node.id)
                .or_default()
                .push(ProjectionCandidate {
                    graph_index,
                    marker,
                    raw_label,
                });
        }
    }

    for (id, group) in candidates {
        if !group.iter().any(|candidate| candidate.marker) {
            continue;
        }
        let lineage_anchor = lineage_root
            .filter(|root| group.iter().any(|candidate| candidate.graph_index == *root));

        if let Some(anchor) = lineage_anchor {
            let anchor_raw_label = group
                .iter()
                .find(|candidate| candidate.graph_index == anchor)
                .and_then(|candidate| candidate.raw_label.clone());
            for candidate in group {
                if candidate.marker || candidate.raw_label == anchor_raw_label {
                    maps[candidate.graph_index].insert(id, NodeIdentity::Projected(id, anchor));
                }
            }
        } else {
            for candidate in group.into_iter().filter(|candidate| candidate.marker) {
                maps[candidate.graph_index]
                    .insert(id, NodeIdentity::Projected(id, candidate.graph_index));
            }
        }
    }

    maps
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
        NodeIdentity::Projected(id, _) => (id.get(), true),
        NodeIdentity::Label(label) => (content_id(label), false),
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
