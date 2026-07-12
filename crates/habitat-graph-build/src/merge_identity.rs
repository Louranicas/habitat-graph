use std::collections::{HashMap, HashSet};

use habitat_graph_core::{content_id, is_canonical_redaction_marker, Graph, Node, NodeId};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum NodeIdentity {
    Stable(NodeId),
    Label(String),
}

pub(crate) fn redacted_node_ids(graphs: &[&Graph]) -> HashSet<NodeId> {
    graphs
        .iter()
        .flat_map(|graph| &graph.nodes)
        .filter(|node| is_canonical_redaction_marker(&node.label))
        .map(|node| node.id)
        .collect()
}

pub(crate) fn node_identity(node: &Node, redacted_ids: &HashSet<NodeId>) -> NodeIdentity {
    if is_canonical_redaction_marker(&node.label)
        || (redacted_ids.contains(&node.id) && content_id(&node.label) == node.id.get())
    {
        NodeIdentity::Stable(node.id)
    } else {
        NodeIdentity::Label(node.label.clone())
    }
}

pub(crate) fn node_identity_set(
    nodes: &[Node],
    redacted_ids: &HashSet<NodeId>,
) -> HashSet<NodeIdentity> {
    nodes
        .iter()
        .map(|node| node_identity(node, redacted_ids))
        .collect()
}

pub(crate) fn node_id_to_identity_map(
    nodes: &[Node],
    redacted_ids: &HashSet<NodeId>,
) -> HashMap<NodeId, NodeIdentity> {
    nodes
        .iter()
        .map(|node| (node.id, node_identity(node, redacted_ids)))
        .collect()
}
