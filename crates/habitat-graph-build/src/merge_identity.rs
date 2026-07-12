use std::collections::{HashMap, HashSet};

use habitat_graph_core::{
    content_id, is_canonical_redaction_marker, redact_public_text, Graph, Node, NodeId,
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NodeIdentity {
    Projected(NodeId, String),
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

pub(crate) type RedactedNodeMarkers = HashMap<NodeId, HashSet<String>>;

pub(crate) fn redacted_node_markers(graphs: &[&Graph]) -> RedactedNodeMarkers {
    let mut markers: RedactedNodeMarkers = HashMap::new();
    for node in graphs
        .iter()
        .flat_map(|graph| &graph.nodes)
        .filter(|node| is_canonical_redaction_marker(&node.label))
    {
        markers
            .entry(node.id)
            .or_default()
            .insert(node.label.clone());
    }
    markers
}

pub(crate) fn node_identity(node: &Node, redacted_markers: &RedactedNodeMarkers) -> NodeIdentity {
    if is_canonical_redaction_marker(&node.label) {
        return NodeIdentity::Projected(node.id, node.label.clone());
    }

    let projected = redact_public_text(&node.label);
    if projected.as_ref() != node.label
        && redacted_markers
            .get(&node.id)
            .is_some_and(|markers| markers.contains(projected.as_ref()))
    {
        NodeIdentity::Projected(node.id, projected.into_owned())
    } else {
        NodeIdentity::Label(node.label.clone())
    }
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
    redacted_markers: &RedactedNodeMarkers,
) -> HashSet<NodeIdentity> {
    nodes
        .iter()
        .map(|node| node_identity(node, redacted_markers))
        .collect()
}

pub(crate) fn node_id_to_identity_map(
    nodes: &[Node],
    redacted_markers: &RedactedNodeMarkers,
) -> HashMap<NodeId, NodeIdentity> {
    nodes
        .iter()
        .map(|node| (node.id, node_identity(node, redacted_markers)))
        .collect()
}
