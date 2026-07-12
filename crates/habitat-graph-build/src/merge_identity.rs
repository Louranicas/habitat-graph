use std::collections::{HashMap, HashSet};

use habitat_graph_core::{is_canonical_redaction_marker, redact_public_text, Graph, Node, NodeId};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NodeIdentity {
    Stable(NodeId),
    Label(String),
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
    let matches_public_projection = redacted_markers.get(&node.id).is_some_and(|markers| {
        let projected = redact_public_text(&node.label);
        projected.as_ref() != node.label && markers.contains(projected.as_ref())
    });
    if is_canonical_redaction_marker(&node.label) || matches_public_projection {
        NodeIdentity::Stable(node.id)
    } else {
        NodeIdentity::Label(node.label.clone())
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
