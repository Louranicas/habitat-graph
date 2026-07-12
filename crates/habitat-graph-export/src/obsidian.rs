//! Obsidian vault export: one note per node with `[[wikilinks]]`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as FmtWrite;

use habitat_graph_core::{
    display_safe, is_canonical_redaction_marker, sanitize_label, CommunityId, Graph, Node, NodeId,
};
use unicode_normalization::UnicodeNormalization as _;

use crate::escape::{project_public_edges, redact_public_text};

/// Renders `graph` as an Obsidian vault: a deterministic list of `(filename, markdown)` pairs —
/// one note per node (its `source_file` + `[[wikilinks]]` to connected nodes) plus a
/// Map-of-Content index note (`_MOC.md`). The caller is responsible for writing the files.
/// Infallible.
///
/// # Filename rules
///
/// Each node filename is derived from its label via [`sanitize_label`] (strips control chars,
/// caps at 256), then replacing path/Obsidian-link metacharacters and whitespace with `_`, then
/// appending `.md`. Secret-bearing labels use a filesystem-safe `REDACTED_<tags>_<NodeId>` stem so
/// their filenames remain stable as other redacted nodes are added. When two or more clean nodes
/// produce the same stem both are disambiguated by appending the [`NodeId`] before the extension
/// (e.g. `foo_n1.md`, `foo_n2.md`). Portable filename equivalence includes case folding and
/// Unicode compatibility normalization; Windows-reserved stems are always qualified.
///
/// # Note content
///
/// ```text
/// # {display_safe(label)}
///
/// Source: `{source_file}`
///
/// ## Links
/// - {relation} [[{assigned_filename_stem}|{display_safe(neighbour_label)}]]
/// …
/// ```
///
/// An edge appears at **both** endpoints (source and target). Self-loops appear once.
///
/// # MOC
///
/// `_MOC.md` lists every node under `## community {id}` headings (sorted by `CommunityId`);
/// nodes that belong to no community appear under `## Unclustered`.
///
/// All pairs are returned sorted by filename for determinism.
#[must_use]
pub fn render_vault(graph: &Graph) -> Vec<(String, String)> {
    let label_map = collect_labels(graph);
    let filenames = assign_filenames(graph);
    let filename_map: HashMap<NodeId, String> = graph
        .nodes
        .iter()
        .zip(filenames.iter())
        .map(|(node, filename)| (node.id, filename.clone()))
        .collect();
    let adj = build_adjacency(graph);
    let node_to_comm = node_community_map(graph);

    let mut result: Vec<(String, String)> = graph
        .nodes
        .iter()
        .zip(filenames.iter())
        .map(|(node, fname)| {
            (
                fname.clone(),
                render_node_note(node, &adj, &label_map, &filename_map, &node_to_comm),
            )
        })
        .collect();

    result.push((
        "_MOC.md".to_owned(),
        render_moc(graph, &label_map, &filename_map),
    ));
    result.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Builds a `NodeId → CommunityId` map (a node belongs to at most one community).
#[must_use]
fn node_community_map(graph: &Graph) -> HashMap<NodeId, CommunityId> {
    let mut map = HashMap::new();
    for community in &graph.communities {
        for &member in &community.members {
            map.insert(member, community.id);
        }
    }
    map
}

/// Derives the owning crate from a `crates/<name>/…` source path (else the first path segment, else
/// `"root"`). Used for graph-view colour groups + a `crate/<name>` tag.
#[must_use]
fn crate_of(source_file: &str) -> String {
    let parts: Vec<&str> = source_file.split('/').collect();
    if parts.first() == Some(&"crates") && parts.len() >= 2 {
        sanitize_label(parts[1])
    } else if let Some(first) = parts.first().filter(|s| !s.is_empty()) {
        sanitize_label(first)
    } else {
        "root".to_owned()
    }
}

/// Maps a source-file extension to a coarse language tag.
#[must_use]
fn lang_of(source_file: &str) -> &'static str {
    match source_file.rsplit('.').next() {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js" | "ts" | "tsx" | "jsx") => "js",
        _ => "other",
    }
}

/// Returns a `NodeId → label` look-up for the graph (clones labels for owned storage).
#[must_use]
fn collect_labels(graph: &Graph) -> HashMap<NodeId, String> {
    graph
        .nodes
        .iter()
        .map(|n| (n.id, redact_public_text(&n.label).into_owned()))
        .collect()
}

/// Assigns each node (in `graph.nodes` order) a `.md` filename based on the sanitized label,
/// disambiguating portable stem collisions by appending the [`NodeId`].
#[must_use]
fn assign_filenames(graph: &Graph) -> Vec<String> {
    let stems: Vec<String> = graph.nodes.iter().map(|n| make_stem(&n.label)).collect();
    let stem_keys: Vec<String> = stems
        .iter()
        .map(|stem| portable_filename_key(&format!("{stem}.md")))
        .collect();

    let mut stem_count: HashMap<&str, usize> = HashMap::new();
    for key in &stem_keys {
        *stem_count.entry(key.as_str()).or_default() += 1;
    }

    let qualified: Vec<bool> = graph
        .nodes
        .iter()
        .zip(stems.iter())
        .zip(stem_keys.iter())
        .map(|((node, stem), stem_key)| {
            let projected = redact_public_text(&node.label);
            is_canonical_redaction_marker(&projected)
                || stem_count.get(stem_key.as_str()).copied().unwrap_or(0) > 1
                || windows_reserved_stem(stem)
        })
        .collect();
    let preferred: Vec<String> = graph
        .nodes
        .iter()
        .zip(stems.iter())
        .zip(qualified.iter())
        .map(|((node, stem), qualified)| {
            if *qualified {
                format!("{}.md", qualified_stem(stem, node.id))
            } else {
                format!("{stem}.md")
            }
        })
        .collect();
    let preferred_keys: Vec<String> = preferred
        .iter()
        .map(|filename| portable_filename_key(filename))
        .collect();

    let mut preferred_count: HashMap<&str, usize> = HashMap::new();
    let moc_key = portable_filename_key("_MOC.md");
    preferred_count.insert(moc_key.as_str(), 1);
    for key in &preferred_keys {
        *preferred_count.entry(key.as_str()).or_default() += 1;
    }
    let reserved: HashSet<&str> = preferred_count.keys().copied().collect();
    let mut assigned = vec![String::new(); graph.nodes.len()];
    let mut used = HashSet::from([moc_key.clone()]);
    let mut order: Vec<usize> = (0..graph.nodes.len()).collect();
    order.sort_unstable_by_key(|index| (!qualified[*index], graph.nodes[*index].id, *index));

    for index in order {
        let candidate = &preferred[index];
        let candidate_key = &preferred_keys[index];
        if !used.contains(candidate_key)
            && (qualified[index] || preferred_count.get(candidate_key.as_str()) == Some(&1))
        {
            assigned[index].clone_from(candidate);
            used.insert(candidate_key.clone());
            continue;
        }

        let base = qualified_stem(&stems[index], graph.nodes[index].id);
        let mut attempt = 1_usize;
        loop {
            let fallback = if attempt == 1 {
                format!("{base}.md")
            } else {
                format!("{base}_{attempt}.md")
            };
            let fallback_key = portable_filename_key(&fallback);
            if !used.contains(&fallback_key) && !reserved.contains(fallback_key.as_str()) {
                used.insert(fallback_key);
                assigned[index] = fallback;
                break;
            }
            attempt = attempt.saturating_add(1);
        }
    }

    assigned
}

fn portable_filename_key(filename: &str) -> String {
    filename.nfkd().flat_map(char::to_lowercase).collect()
}

fn qualified_stem(stem: &str, id: NodeId) -> String {
    if let Some(separator) = stem.find('.').filter(|_| windows_reserved_stem(stem)) {
        format!("{}_{}{}", &stem[..separator], id, &stem[separator..])
    } else {
        format!("{stem}_{id}")
    }
}

fn windows_reserved_stem(stem: &str) -> bool {
    let basename: String = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .nfkc()
        .collect();
    let basename = basename.to_ascii_uppercase();
    matches!(basename.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || basename
            .strip_prefix("COM")
            .or_else(|| basename.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

/// Builds a per-node sorted adjacency list: `NodeId → [(relation, neighbour_id)]`.
///
/// Every edge contributes an entry for **both** endpoints; self-loops contribute one entry
/// (at the source side) to avoid double-counting a single edge.
#[must_use]
fn build_adjacency(graph: &Graph) -> HashMap<NodeId, Vec<(String, NodeId)>> {
    let mut adj: HashMap<NodeId, Vec<(String, NodeId)>> = HashMap::new();
    for projected in project_public_edges(graph) {
        let edge = projected.edge;
        let relation = projected.relation;
        adj.entry(edge.source)
            .or_default()
            .push((relation.clone(), edge.target));
        if edge.source != edge.target {
            adj.entry(edge.target)
                .or_default()
                .push((relation, edge.source));
        }
    }
    for neighbours in adj.values_mut() {
        neighbours.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    }
    adj
}

/// Keeps only characters safe for an unquoted YAML scalar / tag token (alphanumeric plus `._-`).
///
/// Prevents a crafted path segment in `crate:`/`crate/<x>` from injecting a YAML mapping artifact
/// or breaking the `tags: [...]` array (STRIDE-T hardening).
fn yaml_token(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .collect()
}

/// Escapes a string for embedding inside a double-quoted YAML scalar.
///
/// Escapes backslash and double-quote (after [`display_safe`] has already removed control/bidi
/// codepoints) so a `"` in a path cannot close the quoted scalar early and corrupt the frontmatter
/// (STRIDE-T hardening).
fn yaml_dq(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Sanitises an edge relation for use as a Dataview inline-field key (`relation:: [[..]]`).
///
/// Applies [`display_safe`] (neutralising Trojan-Source bidi overrides and control characters),
/// then strips the metacharacters that would break the field or inject a spurious wikilink
/// (`[`, `]`, `:`) (STRIDE-T hardening — closes the raw-`relation` boundary that `node.label`
/// already guards).
fn field_key(relation: &str) -> String {
    display_safe(relation)
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':'))
        .collect()
}

/// Renders the Markdown content for one node note — YAML frontmatter (for Dataview / Juggl / the
/// graph-view colour groups) + a `## Links` section whose edges are Dataview inline fields
/// (`relation:: [[target]]`, also consumed by Breadcrumbs) so each typed edge is queryable.
#[must_use]
fn render_node_note(
    node: &Node,
    adj: &HashMap<NodeId, Vec<(String, NodeId)>>,
    label_map: &HashMap<NodeId, String>,
    filename_map: &HashMap<NodeId, String>,
    node_to_comm: &HashMap<NodeId, CommunityId>,
) -> String {
    let redacted_label = redact_public_text(&node.label);
    let redacted_file = redact_public_text(&node.source_file);
    let safe_label = wikilink_alias(&redacted_label);
    let krate = yaml_token(&crate_of(&redacted_file));
    let lang = lang_of(&redacted_file);
    let line = node.source_location.start_line;
    let degree = adj.get(&node.id).map_or(0, Vec::len);
    let community = node_to_comm.get(&node.id).map(|c| c.get());

    let mut content = String::from("---\n");
    let _ = writeln!(content, "id: {}", node.id.get());
    if let Some(cid) = community {
        let _ = writeln!(content, "community: {cid}");
    }
    let _ = writeln!(content, "crate: {krate}");
    let _ = writeln!(content, "lang: {lang}");
    let _ = writeln!(
        content,
        "file: \"{}\"",
        yaml_dq(&display_safe(&redacted_file))
    );
    let _ = writeln!(content, "line: {line}");
    let _ = writeln!(content, "degree: {degree}");
    // Tags drive graph-view colour groups (`tag:#crate/<name>`) + Dataview `FROM #community/<n>`.
    content.push_str("tags: [hg/node");
    let _ = write!(content, ", crate/{krate}, lang/{lang}");
    if let Some(cid) = community {
        let _ = write!(content, ", community/{cid}");
    }
    content.push_str("]\n---\n\n");

    let _ = writeln!(content, "# {safe_label}\n");
    let _ = writeln!(
        content,
        "> `{}:{line}` · crate `{krate}` · degree {degree}\n",
        display_safe(&redacted_file)
    );
    content.push_str("## Links\n");
    if let Some(neighbours) = adj.get(&node.id) {
        for (relation, neighbour_id) in neighbours {
            let neighbour_link = render_wikilink(*neighbour_id, label_map, filename_map);
            // `relation:: [[x]]` = a Dataview inline field (queryable typed edge) + Breadcrumbs relation.
            // `relation` is attacker-influenced → field_key strips bidi/controls + `[`/`]`/`:` so it
            // cannot inject a spurious wikilink or break the field (STRIDE-T, parity with node.label).
            // write! on String is infallible (OOM is the only failure, which aborts).
            let safe_relation = field_key(relation);
            let _ = writeln!(content, "- {safe_relation}:: {neighbour_link}");
        }
    }
    content
}

/// Renders the `_MOC.md` Map-of-Content note grouping every node by community.
#[must_use]
fn render_moc(
    graph: &Graph,
    label_map: &HashMap<NodeId, String>,
    filename_map: &HashMap<NodeId, String>,
) -> String {
    // Map NodeId → CommunityId for grouping.
    let mut node_to_comm: HashMap<NodeId, CommunityId> = HashMap::new();
    for community in &graph.communities {
        for &member in &community.members {
            node_to_comm.insert(member, community.id);
        }
    }

    let mut comm_members: BTreeMap<CommunityId, Vec<NodeId>> = BTreeMap::new();
    let mut unclustered: Vec<NodeId> = Vec::new();

    for node in &graph.nodes {
        match node_to_comm.get(&node.id) {
            Some(&cid) => comm_members.entry(cid).or_default().push(node.id),
            None => unclustered.push(node.id),
        }
    }

    for members in comm_members.values_mut() {
        members.sort_unstable();
    }

    let mut moc = String::from("# Map of Content\n");

    for (comm_id, members) in &comm_members {
        let _ = write!(moc, "\n## community {comm_id}\n\n");
        for &nid in members {
            let _ = writeln!(moc, "- {}", render_wikilink(nid, label_map, filename_map));
        }
    }

    if !unclustered.is_empty() {
        moc.push_str("\n## Unclustered\n\n");
        for nid in &unclustered {
            let _ = writeln!(moc, "- {}", render_wikilink(*nid, label_map, filename_map));
        }
    }

    moc
}

/// Renders an Obsidian wikilink using the assigned filename as the target and the redacted label
/// as an optional display alias. Filename identity keeps colliding redaction markers distinct.
fn render_wikilink(
    node_id: NodeId,
    label_map: &HashMap<NodeId, String>,
    filename_map: &HashMap<NodeId, String>,
) -> String {
    let label = label_map.get(&node_id).map_or("", String::as_str);
    let display = wikilink_alias(label);
    let target = filename_map
        .get(&node_id)
        .and_then(|filename| filename.strip_suffix(".md"))
        .unwrap_or("");
    if target == display {
        format!("[[{target}]]")
    } else {
        format!("[[{target}|{display}]]")
    }
}

fn wikilink_alias(label: &str) -> String {
    let display = display_safe(label);
    let mut escaped = String::with_capacity(display.len());
    for character in display.chars() {
        if matches!(character, '\\' | '[' | ']' | '|') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Converts a node label to a filesystem-safe filename stem (no path/link metacharacters,
/// whitespace, or controls).
///
/// [`sanitize_label`] strips control characters first; this function then replaces `/` and
/// whitespace with `_`. Returns `"_"` if the resulting stem would be empty (all-control label).
#[must_use]
fn make_stem(label: &str) -> String {
    let redacted = redact_public_text(label);
    if is_canonical_redaction_marker(&redacted) {
        let tags = redacted
            .strip_prefix("[REDACTED:")
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or_default();
        return format!("REDACTED_{}", tags.replace(',', "_"));
    }
    let sanitized = display_safe(&sanitize_label(&redacted));
    let stem: String = sanitized
        .chars()
        .map(|c| {
            if c.is_whitespace()
                || matches!(
                    c,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '[' | ']' | '#' | '^'
                )
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    if stem.is_empty() {
        String::from("_")
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Manifest, Node, NodeId, Span,
    };

    use super::render_vault;

    // ── Test fixtures ───────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 1, 1, 1)
    }

    fn make_node(id: u32, label: &str, source_file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: source_file.to_owned(),
            source_location: span(),
        }
    }

    fn make_edge(source: u32, target: u32, relation: &str) -> Edge {
        Edge {
            source: NodeId::new(source),
            target: NodeId::new(target),
            relation: relation.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn make_community(id: u32, label: &str, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: label.to_owned(),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    fn empty_manifest() -> Manifest {
        Manifest {
            inputs: Vec::new(),
            tool_version: "test".to_owned(),
            generated_at: None,
        }
    }

    fn graph_with_nodes(nodes: Vec<Node>) -> Graph {
        Graph {
            schema: "test".to_owned(),
            nodes,
            edges: Vec::new(),
            communities: Vec::new(),
            manifest: empty_manifest(),
        }
    }

    // ── Tests ───────────────────────────────────────────────────────────────

    /// T01: empty graph → exactly one pair: the MOC note.
    #[test]
    fn empty_graph_returns_only_moc() {
        let g = Graph::default();
        let pairs = render_vault(&g);
        assert_eq!(pairs.len(), 1, "expected exactly 1 pair");
        assert_eq!(pairs[0].0, "_MOC.md");
        assert_eq!(pairs[0].1, "# Map of Content\n");
    }

    /// T02: single node → two pairs (the node note plus `_MOC.md`).
    #[test]
    fn single_node_yields_two_pairs() {
        let mut g = graph_with_nodes(vec![make_node(1, "alpha", "src/lib.rs")]);
        g.communities.push(make_community(0, "c0", &[1]));
        let pairs = render_vault(&g);
        assert_eq!(pairs.len(), 2);
    }

    /// T03: the note filename is the sanitized label + `.md`.
    #[test]
    fn note_filename_matches_sanitized_label() {
        let g = graph_with_nodes(vec![make_node(1, "mynode", "f.rs")]);
        let pairs = render_vault(&g);
        let fname = pairs
            .iter()
            .find(|(f, _)| f != "_MOC.md")
            .map(|(f, _)| f.as_str());
        assert_eq!(fname, Some("mynode.md"));
    }

    /// T04: the note content includes the source file in a code span.
    #[test]
    fn note_contains_source_file() {
        let g = graph_with_nodes(vec![make_node(1, "alpha", "crates/foo/src/lib.rs")]);
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "alpha.md").unwrap();
        assert!(
            content.contains("crates/foo/src/lib.rs"),
            "source file missing from note: {content}"
        );
    }

    /// T05: the note heading is `# {label}`.
    #[test]
    fn note_heading_contains_label() {
        let g = graph_with_nodes(vec![make_node(1, "myfn", "a.rs")]);
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "myfn.md").unwrap();
        assert!(content.contains("# myfn\n"), "bad heading: {content}");
    }

    /// T06: the note always has a `## Links` section, even with no edges.
    #[test]
    fn note_has_links_section_header() {
        let g = graph_with_nodes(vec![make_node(1, "solo", "s.rs")]);
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "solo.md").unwrap();
        assert!(
            content.contains("## Links\n"),
            "## Links missing: {content}"
        );
    }

    /// T07: an outgoing edge creates a wikilink to the target in the source node's note.
    #[test]
    fn wikilink_for_out_edge() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "alpha.md").unwrap();
        assert!(
            content.contains("- calls:: [[beta]]"),
            "wikilink missing: {content}"
        );
    }

    /// T07b: node notes carry YAML frontmatter (community/crate/lang/degree + tags) for Dataview,
    /// Juggl, and the graph-view colour groups.
    #[test]
    fn note_has_frontmatter_and_tags() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "crates/habitat-graph-core/src/schema.rs"),
            make_node(2, "beta", "b.py"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        g.communities.push(make_community(7, "c7", &[1]));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "alpha.md").unwrap();
        assert!(content.starts_with("---\n"), "no frontmatter: {content}");
        assert!(content.contains("community: 7"), "{content}");
        assert!(content.contains("crate: habitat-graph-core"), "{content}");
        assert!(content.contains("lang: rust"), "{content}");
        assert!(content.contains("degree: 1"), "{content}");
        assert!(
            content.contains("tags: [hg/node, crate/habitat-graph-core, lang/rust, community/7]"),
            "tags wrong: {content}"
        );
    }

    /// T07c: a Python source file is tagged `lang/python`; a node with no community omits the field.
    #[test]
    fn lang_python_and_no_community() {
        let g = graph_with_nodes(vec![make_node(2, "beta", "pkg/mod.py")]);
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "beta.md").unwrap();
        assert!(content.contains("lang: python"), "{content}");
        assert!(content.contains("crate: pkg"), "{content}");
        assert!(
            !content.contains("community:"),
            "should omit community: {content}"
        );
    }

    /// T08: the target node of an edge also sees a link back to the source node.
    #[test]
    fn wikilink_for_in_edge() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "beta.md").unwrap();
        assert!(
            content.contains("- calls:: [[alpha]]"),
            "in-edge wikilink missing from beta: {content}"
        );
    }

    /// T09: a self-loop edge appears exactly once in the node's links section.
    #[test]
    fn self_loop_counted_once() {
        let mut g = graph_with_nodes(vec![make_node(1, "recursive", "r.rs")]);
        g.edges.push(make_edge(1, 1, "recurses"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "recursive.md").unwrap();
        let count = content.matches("- recurses:: [[recursive]]").count();
        assert_eq!(count, 1, "self-loop appeared {count} times, expected 1");
    }

    /// T10: the MOC note filename is exactly `_MOC.md`.
    #[test]
    fn moc_filename_is_moc_md() {
        let g = Graph::default();
        let pairs = render_vault(&g);
        assert!(
            pairs.iter().any(|(f, _)| f == "_MOC.md"),
            "no _MOC.md in result"
        );
    }

    /// T11: the MOC note lists every node in the graph.
    #[test]
    fn moc_lists_all_nodes() {
        let g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
            make_node(3, "gamma", "c.rs"),
        ]);
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        assert!(moc.contains("[[alpha]]"), "alpha missing from MOC");
        assert!(moc.contains("[[beta]]"), "beta missing from MOC");
        assert!(moc.contains("[[gamma]]"), "gamma missing from MOC");
    }

    /// T12: with lowercase labels `_MOC.md` sorts lexicographically first (`_` < `a`).
    #[test]
    fn moc_sorts_first_with_lowercase_labels() {
        let g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        let pairs = render_vault(&g);
        assert_eq!(pairs[0].0, "_MOC.md", "MOC should be first: {pairs:?}");
    }

    /// T13: `/` in a label becomes `_` in the filename.
    #[test]
    fn slash_in_label_becomes_underscore() {
        let g = graph_with_nodes(vec![make_node(1, "foo/bar", "x.rs")]);
        let pairs = render_vault(&g);
        assert!(
            pairs.iter().any(|(f, _)| f == "foo_bar.md"),
            "expected foo_bar.md, got: {pairs:?}"
        );
        assert!(
            !pairs.iter().any(|(f, _)| f.contains('/')),
            "raw slash leaked into filename"
        );
    }

    /// T14: whitespace in a label becomes `_` in the filename.
    #[test]
    fn whitespace_in_label_becomes_underscore() {
        let g = graph_with_nodes(vec![make_node(1, "foo bar", "x.rs")]);
        let pairs = render_vault(&g);
        assert!(
            pairs.iter().any(|(f, _)| f == "foo_bar.md"),
            "expected foo_bar.md: {pairs:?}"
        );
        let note_fname = pairs
            .iter()
            .find(|(f, _)| f != "_MOC.md")
            .map(|(f, _)| f.as_str());
        assert!(
            note_fname.is_none_or(|f| !f.contains(' ')),
            "whitespace leaked into filename"
        );
    }

    /// T15: two nodes whose labels produce the same stem are both disambiguated with the `NodeId`.
    #[test]
    fn collision_appends_node_id() {
        // "foo/bar" and "foo bar" both become stem "foo_bar".
        let g = graph_with_nodes(vec![
            make_node(1, "foo/bar", "a.rs"),
            make_node(2, "foo bar", "b.rs"),
        ]);
        let pairs = render_vault(&g);
        let fnames: Vec<&str> = pairs
            .iter()
            .filter(|(f, _)| f != "_MOC.md")
            .map(|(f, _)| f.as_str())
            .collect();
        // Both must carry NodeId suffix (n1 / n2).
        assert!(
            fnames.contains(&"foo_bar_n1.md"),
            "expected foo_bar_n1.md in {fnames:?}"
        );
        assert!(
            fnames.contains(&"foo_bar_n2.md"),
            "expected foo_bar_n2.md in {fnames:?}"
        );
    }

    #[test]
    fn filenames_are_unique_on_case_insensitive_filesystems() {
        let g = graph_with_nodes(vec![
            make_node(1, "Foo", "a.rs"),
            make_node(2, "foo", "b.rs"),
            make_node(3, "_moc", "c.rs"),
            make_node(4, "CON", "d.rs"),
        ]);
        let pairs = render_vault(&g);
        let filenames: Vec<&str> = pairs
            .iter()
            .map(|(filename, _)| filename.as_str())
            .collect();
        assert!(filenames.contains(&"Foo_n1.md"));
        assert!(filenames.contains(&"foo_n2.md"));
        assert!(filenames.contains(&"_moc_n3.md"));
        assert!(filenames.contains(&"CON_n4.md"));

        let keys: HashSet<String> = filenames
            .iter()
            .map(|filename| super::portable_filename_key(filename))
            .collect();
        assert_eq!(keys.len(), filenames.len());
    }

    #[test]
    fn windows_reserved_stems_with_extensions_are_qualified_before_the_dot() {
        let g = graph_with_nodes(vec![
            make_node(1, "CON.txt", "a.rs"),
            make_node(2, "NUL.foo", "b.rs"),
            make_node(3, "COM1.rs", "c.rs"),
            make_node(4, "LPT¹.log", "d.rs"),
        ]);
        let pairs = render_vault(&g);
        let filenames: Vec<&str> = pairs
            .iter()
            .map(|(filename, _)| filename.as_str())
            .collect();
        assert!(filenames.contains(&"CON_n1.txt.md"));
        assert!(filenames.contains(&"NUL_n2.foo.md"));
        assert!(filenames.contains(&"COM1_n3.rs.md"));
        assert!(filenames.contains(&"LPT¹_n4.log.md"));
        assert!(filenames
            .iter()
            .filter(|filename| **filename != "_MOC.md")
            .all(|filename| !super::windows_reserved_stem(filename)));
    }

    #[test]
    fn filenames_are_unique_across_unicode_normalization_forms() {
        let g = graph_with_nodes(vec![
            make_node(1, "Caf\u{e9}", "a.rs"),
            make_node(2, "Cafe\u{301}", "b.rs"),
        ]);
        let pairs = render_vault(&g);
        let filenames: Vec<&str> = pairs
            .iter()
            .map(|(filename, _)| filename.as_str())
            .collect();
        let keys: HashSet<String> = filenames
            .iter()
            .map(|filename| super::portable_filename_key(filename))
            .collect();
        assert_eq!(keys.len(), filenames.len());
        assert!(filenames.iter().all(|filename| {
            *filename == "_MOC.md" || filename.contains("_n1") || filename.contains("_n2")
        }));
    }

    #[test]
    fn filenames_are_unique_across_unicode_compatibility_forms() {
        let g = graph_with_nodes(vec![
            make_node(1, "\u{fb00}oo", "a.rs"),
            make_node(2, "ffoo", "b.rs"),
        ]);
        let pairs = render_vault(&g);
        let filenames: Vec<&str> = pairs
            .iter()
            .map(|(filename, _)| filename.as_str())
            .collect();
        let keys: HashSet<String> = filenames
            .iter()
            .map(|filename| super::portable_filename_key(filename))
            .collect();
        assert_eq!(keys.len(), filenames.len());
    }

    /// T16: output is sorted lexicographically by filename.
    #[test]
    fn output_is_sorted_by_filename() {
        let g = graph_with_nodes(vec![
            make_node(3, "zeta", "z.rs"),
            make_node(1, "alpha", "a.rs"),
            make_node(2, "mu", "m.rs"),
        ]);
        let pairs = render_vault(&g);
        let fnames: Vec<&str> = pairs.iter().map(|(f, _)| f.as_str()).collect();
        let mut sorted = fnames.clone();
        sorted.sort_unstable();
        assert_eq!(fnames, sorted, "output is not sorted");
    }

    /// T17: calling `render_vault` twice on the same graph yields identical results.
    #[test]
    fn calling_twice_is_idempotent() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let first = render_vault(&g);
        let second = render_vault(&g);
        assert_eq!(first, second, "render_vault is not deterministic");
    }

    /// T18: labels containing bidi / control characters are `display_safe`-escaped in note content.
    #[test]
    fn bidi_chars_escaped_in_note_content() {
        // U+202E RIGHT-TO-LEFT OVERRIDE — classic Trojan-Source codepoint.
        let evil = "fn\u{202E}mal".to_owned();
        let g = graph_with_nodes(vec![make_node(1, &evil, "src.rs")]);
        let pairs = render_vault(&g);
        // The heading must not contain the raw U+202E.
        let (_, content) = pairs.iter().find(|(f, _)| f != "_MOC.md").unwrap();
        assert!(
            !content.contains('\u{202E}'),
            "raw bidi char leaked into note: {content:?}"
        );
        assert!(
            content.contains("\\u{202E}"),
            "escaped form missing from note: {content:?}"
        );
    }

    /// T19: bidi chars in neighbour labels are also escaped in wikilinks.
    #[test]
    fn bidi_chars_escaped_in_wikilink() {
        let evil = "b\u{202E}eta".to_owned();
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, &evil, "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "alpha.md").unwrap();
        assert!(
            !content.contains('\u{202E}'),
            "raw bidi char in wikilink: {content:?}"
        );
    }

    /// T20: bidi chars in labels are also escaped in the MOC.
    #[test]
    fn bidi_chars_escaped_in_moc() {
        let evil = "a\u{202E}lpha".to_owned();
        let g = graph_with_nodes(vec![make_node(1, &evil, "a.rs")]);
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        assert!(
            !moc.contains('\u{202E}'),
            "raw bidi char leaked into MOC: {moc:?}"
        );
    }

    /// T21: nodes in a community appear under the correct `## community {id}` heading.
    #[test]
    fn moc_groups_by_community() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.communities.push(make_community(0, "cluster-a", &[1, 2]));
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        assert!(
            moc.contains("## community c0"),
            "community heading missing: {moc}"
        );
        assert!(moc.contains("[[alpha]]"), "alpha missing under community");
        assert!(moc.contains("[[beta]]"), "beta missing under community");
    }

    /// T22: a node not assigned to any community appears under `## Unclustered`.
    #[test]
    fn unclustered_node_in_moc() {
        let g = graph_with_nodes(vec![make_node(1, "orphan", "o.rs")]);
        // No communities added.
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        assert!(
            moc.contains("## Unclustered"),
            "Unclustered section missing: {moc}"
        );
        assert!(
            moc.contains("[[orphan]]"),
            "orphan missing from Unclustered: {moc}"
        );
    }

    /// T23: multiple communities appear in ascending `CommunityId` order in the MOC.
    #[test]
    fn multiple_communities_ordered_in_moc() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
            make_node(3, "gamma", "c.rs"),
        ]);
        // Insert communities deliberately out of id order.
        g.communities.push(make_community(2, "last", &[3]));
        g.communities.push(make_community(0, "first", &[1]));
        g.communities.push(make_community(1, "mid", &[2]));
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        let pos_c0 = moc.find("## community c0").unwrap_or(usize::MAX);
        let pos_c1 = moc.find("## community c1").unwrap_or(usize::MAX);
        let pos_c2 = moc.find("## community c2").unwrap_or(usize::MAX);
        assert!(
            pos_c0 < pos_c1 && pos_c1 < pos_c2,
            "communities out of order in MOC:\n{moc}"
        );
    }

    /// T24: the edge relation string appears verbatim in the link bullet.
    #[test]
    fn edge_relation_appears_in_links() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "imports"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "alpha.md").unwrap();
        assert!(
            content.contains("- imports:: [[beta]]"),
            "relation 'imports' missing: {content}"
        );
    }

    /// T25: a label composed entirely of control characters falls back to `_.md` filename.
    #[test]
    fn all_control_char_label_fallback_filename() {
        // sanitize_label strips all control chars → empty stem → fallback "_".
        let g = graph_with_nodes(vec![make_node(1, "\x01\x02\x03", "ctrl.rs")]);
        let pairs = render_vault(&g);
        assert!(
            pairs.iter().any(|(f, _)| f == "_.md"),
            "fallback filename '_.md' not found: {pairs:?}"
        );
    }

    /// T26: multiple edges touching the same node all appear in its links section.
    #[test]
    fn multiple_edges_all_appear_in_links() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "hub", "h.rs"),
            make_node(2, "alpha", "a.rs"),
            make_node(3, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "calls"));
        g.edges.push(make_edge(1, 3, "imports"));
        let pairs = render_vault(&g);
        let (_, content) = pairs.iter().find(|(f, _)| f == "hub.md").unwrap();
        assert!(
            content.contains("[[alpha]]"),
            "alpha missing from hub: {content}"
        );
        assert!(
            content.contains("[[beta]]"),
            "beta missing from hub: {content}"
        );
    }

    /// T27: nodes not assigned to a community do not appear under a community heading.
    #[test]
    fn community_heading_only_for_assigned_nodes() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "orphan", "o.rs"),
        ]);
        g.communities.push(make_community(0, "c0", &[1]));
        let pairs = render_vault(&g);
        let (_, moc) = pairs.iter().find(|(f, _)| f == "_MOC.md").unwrap();
        // "orphan" must appear under Unclustered, not under community c0.
        let c0_pos = moc.find("## community c0").unwrap_or(usize::MAX);
        let unclust_pos = moc.find("## Unclustered").unwrap_or(usize::MAX);
        let orphan_pos = moc.find("[[orphan]]").unwrap_or(usize::MAX);
        assert!(
            orphan_pos > unclust_pos,
            "orphan should be after Unclustered heading"
        );
        // Also verify orphan is not under c0.
        let alpha_pos = moc.find("[[alpha]]").unwrap_or(usize::MAX);
        assert!(
            alpha_pos > c0_pos && alpha_pos < unclust_pos,
            "alpha should be under community c0"
        );
    }

    // ── Security-hardening regression tests (S1008901 PB security sweep) ──────────

    #[test]
    fn field_key_strips_wikilink_injection_brackets() {
        let out = super::field_key("]] [[INJECTED]] x");
        assert!(!out.contains('['), "field_key must strip '[': {out}");
        assert!(!out.contains(']'), "field_key must strip ']': {out}");
    }

    #[test]
    fn field_key_neutralises_trojan_source_bidi() {
        // U+202E RIGHT-TO-LEFT OVERRIDE must never reach the rendered vault.
        let out = super::field_key("calls\u{202e}evil");
        assert!(
            !out.contains('\u{202e}'),
            "bidi override must be removed: {out:?}"
        );
    }

    #[test]
    fn field_key_preserves_ordinary_relations() {
        assert_eq!(super::field_key("imports_from"), "imports_from");
        assert_eq!(super::field_key("calls"), "calls");
    }

    #[test]
    fn redacted_parallel_relations_keep_distinct_field_keys() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "api_key=alpha"));
        g.edges.push(make_edge(1, 2, "api_key=beta"));

        let rendered = render_vault(&g.sorted());
        let (_, content) = rendered
            .iter()
            .find(|(filename, _)| filename == "alpha.md")
            .unwrap();
        assert!(content.contains("REDACTEDapi_key#e00000000000000000000"));
        assert!(content.contains("REDACTEDapi_key#e00000000000000000001"));
    }

    #[test]
    fn field_key_normalization_cannot_reconstruct_secrets() {
        let mut g = graph_with_nodes(vec![
            make_node(1, "alpha", "a.rs"),
            make_node(2, "beta", "b.rs"),
        ]);
        g.edges.push(make_edge(1, 2, "xox[b]-123-secret"));
        g.edges
            .push(make_edge(1, 2, "-----BEG[IN] OPENSSH PRIVATE KEY-----"));

        let rendered = render_vault(&g.sorted());
        let joined = rendered
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<String>();
        assert!(!joined.contains("xoxb-123-secret"));
        assert!(!joined.contains("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(joined.contains("REDACTEDslack_token#e"));
        assert!(joined.contains("REDACTEDprivate_key#e"));
    }

    #[test]
    fn wikilink_alias_escapes_delimiters_and_backslashes() {
        assert_eq!(
            super::wikilink_alias(r"a\b|c]] [[injected"),
            r"a\\b\|c\]\] \[\[injected"
        );
    }

    #[test]
    fn yaml_dq_escapes_quote_and_backslash() {
        assert_eq!(super::yaml_dq("a\"b\\c"), "a\\\"b\\\\c");
    }

    #[test]
    fn yaml_token_strips_unsafe_yaml_chars() {
        // ':' ',' ']' would break an unquoted scalar or the `tags: [...]` array.
        assert_eq!(super::yaml_token("ev:il],x"), "evilx");
        assert_eq!(super::yaml_token("my-crate_2.0"), "my-crate_2.0");
    }

    #[test]
    fn render_vault_redacts_secret_patterns_in_names_content_and_relations() {
        let mut g = Graph::new();
        g.nodes
            .push(make_node(1, "api_key_assignment_refused", "src/api_key.rs"));
        g.nodes.push(make_node(2, "safe", "safe.rs"));
        g.edges.push(habitat_graph_core::Edge {
            source: habitat_graph_core::NodeId::new(1),
            target: habitat_graph_core::NodeId::new(2),
            relation: "Authorization: Bearer token".to_owned(),
            confidence: habitat_graph_core::Confidence::Extracted,
        });
        let rendered = render_vault(&g.sorted());
        let filenames = rendered
            .iter()
            .map(|(filename, _)| filename.as_str())
            .collect::<Vec<_>>();
        let joined = rendered
            .iter()
            .map(|(_, content)| content.as_str())
            .collect::<String>();
        assert!(filenames
            .iter()
            .any(|name| name.contains("REDACTED_api_key")));
        assert!(!joined.contains("api_key_assignment_refused"));
        assert!(!joined.contains("src/api_key.rs"));
        assert!(!joined.contains("Authorization: Bearer token"));
        assert!(joined.contains("[REDACTED:api_key]"));
        // Dataview field keys cannot contain `[`/`]`/`:`, so the shared marker is reduced to a
        // safe key while retaining its redaction tag.
        assert!(joined.contains("REDACTEDbearer_token"));
    }

    #[test]
    fn redacted_collision_links_target_distinct_node_id_filenames() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "api_key_alpha", "a.rs"));
        g.nodes.push(make_node(2, "api_key_beta", "b.rs"));
        g.edges.push(make_edge(1, 2, "calls"));

        let rendered = render_vault(&g.sorted());
        let first_name = "REDACTED_api_key_n1.md";
        let second_name = "REDACTED_api_key_n2.md";
        let first = rendered
            .iter()
            .find(|(filename, _)| filename == first_name)
            .map(|(_, content)| content)
            .expect("first redacted note");
        let moc = rendered
            .iter()
            .find(|(filename, _)| filename == "_MOC.md")
            .map(|(_, content)| content)
            .expect("MOC");

        assert!(rendered.iter().any(|(filename, _)| filename == second_name));
        assert!(first.contains(r"[[REDACTED_api_key_n2|\[REDACTED:api_key\]]]"));
        assert!(moc.contains(r"[[REDACTED_api_key_n1|\[REDACTED:api_key\]]]"));
        assert!(moc.contains(r"[[REDACTED_api_key_n2|\[REDACTED:api_key\]]]"));
    }

    #[test]
    fn redacted_filename_is_stable_when_another_marker_is_added() {
        let one = graph_with_nodes(vec![make_node(1, "api_key_alpha", "a.rs")]);
        let one_name = render_vault(&one)
            .into_iter()
            .find(|(filename, _)| filename != "_MOC.md")
            .map(|(filename, _)| filename)
            .unwrap();

        let two = graph_with_nodes(vec![
            make_node(1, "api_key_alpha", "a.rs"),
            make_node(2, "api_key_beta", "b.rs"),
        ]);
        let two_names: Vec<String> = render_vault(&two)
            .into_iter()
            .map(|(filename, _)| filename)
            .collect();

        assert_eq!(one_name, "REDACTED_api_key_n1.md");
        assert!(two_names.contains(&one_name));
    }

    #[test]
    fn render_vault_neutralises_hostile_relation_in_node_note() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha", "src/a.rs"));
        g.nodes.push(make_node(2, "beta", "src/b.rs"));
        g.edges.push(habitat_graph_core::Edge {
            source: habitat_graph_core::NodeId::new(1),
            target: habitat_graph_core::NodeId::new(2),
            relation: "]] [[INJECTED]] x".to_owned(),
            confidence: habitat_graph_core::Confidence::Extracted,
        });
        let joined: String = render_vault(&g.sorted())
            .iter()
            .map(|(_, c)| c.as_str())
            .collect();
        assert!(
            !joined.contains("[[INJECTED"),
            "wikilink injection via relation must be neutralised: {joined}"
        );
    }

    #[test]
    fn render_vault_neutralises_hostile_wikilink_alias() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "alpha", "src/a.rs"));
        g.nodes
            .push(make_node(2, r"target\]] [[INJECTED]]", "src/b.rs"));
        g.edges.push(make_edge(1, 2, "calls"));

        let joined: String = render_vault(&g.sorted())
            .iter()
            .map(|(_, content)| content.as_str())
            .collect();
        assert!(!joined.contains("[[INJECTED]]"));
        assert!(joined.contains(r"target\\\]\] \[\[INJECTED\]\]"));
    }

    #[test]
    fn render_vault_escapes_quote_in_source_file_frontmatter() {
        let mut g = Graph::new();
        g.nodes.push(make_node(1, "n", "src/a\"evil: true.rs"));
        let joined: String = render_vault(&g.sorted())
            .iter()
            .map(|(_, c)| c.as_str())
            .collect();
        // The raw double-quote must be backslash-escaped inside the quoted YAML scalar.
        assert!(
            joined.contains("file: \"src/a\\\"evil: true.rs\""),
            "source_file quote must be YAML-escaped: {joined}"
        );
    }

    #[test]
    fn noncanonical_marker_cannot_create_path_components() {
        let g = graph_with_nodes(vec![make_node(
            1,
            "[REDACTED:x/../../../escape]",
            "src/lib.rs",
        )]);
        let rendered = render_vault(&g);
        let filename = rendered
            .iter()
            .find(|(filename, _)| filename != "_MOC.md")
            .map(|(filename, _)| filename)
            .unwrap();

        assert!(!filename.contains('/'));
        assert!(!filename.contains('\\'));
        assert_eq!(std::path::Path::new(filename).components().count(), 1);
    }

    #[test]
    fn final_filename_allocation_handles_cross_stem_collisions() {
        let g = graph_with_nodes(vec![
            make_node(1, "[REDACTED:aws_access_key_id]", "a.rs"),
            make_node(2, "REDACTED_aws_access_key_id_n1", "b.rs"),
        ]);
        let filenames: Vec<String> = render_vault(&g)
            .into_iter()
            .map(|(filename, _)| filename)
            .collect();

        assert!(filenames.contains(&"REDACTED_aws_access_key_id_n1.md".to_owned()));
        assert!(filenames.contains(&"REDACTED_aws_access_key_id_n1_n2.md".to_owned()));
        let unique: std::collections::HashSet<&str> =
            filenames.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), filenames.len());
    }

    #[test]
    fn node_filename_cannot_replace_the_moc() {
        let g = graph_with_nodes(vec![make_node(4, "_MOC", "a.rs")]);
        let filenames: Vec<String> = render_vault(&g)
            .into_iter()
            .map(|(filename, _)| filename)
            .collect();

        assert_eq!(
            filenames
                .iter()
                .filter(|filename| filename.as_str() == "_MOC.md")
                .count(),
            1
        );
        assert!(filenames.contains(&"_MOC_n4.md".to_owned()));
    }
}
