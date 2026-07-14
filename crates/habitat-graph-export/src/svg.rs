//! SVG exporter — a self-contained, deterministically laid-out graph drawing.
//!
//! [`render_svg`] produces a single `<svg>…</svg>` document with one `<circle>` per node,
//! one `<text>` label per node, and one `<line>` per edge. Layout is fully deterministic (R4):
//! nodes are placed on a circle in the order of `graph.nodes`; angles are derived solely from
//! node index and node count. Community membership drives fill colour via a fixed palette. Every
//! attacker-influenced node label is redacted through the shared public-output policy and routed
//! through [`crate::escape::xml_escape`] (STRIDE-T). No randomness; no system clock.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::fmt::Write as FmtWrite;

use habitat_graph_core::{Graph, NodeId};

use crate::escape::{redact_public_text, xml_escape};

// ── Layout constants ──────────────────────────────────────────────────────────

/// Radius of each node glyph, in SVG user units.
const NODE_RADIUS: f64 = 18.0;

/// Distance from the bottom of a node glyph to the text baseline of its label.
const LABEL_OFFSET: f64 = 14.0;

/// Padding around the outermost ring that accommodates labels and prevents clipping.
const RING_PADDING: f64 = 60.0;

/// Minimum canvas side-length in SVG user units (used for empty and single-node graphs).
const MIN_CANVAS: u32 = 300;

// ── Colour palette ────────────────────────────────────────────────────────────

/// Deterministic fill palette; colour index = `community_id % PALETTE.len()`.
///
/// Colours are taken from the Tableau-10 palette for perceptual distinctness.
const PALETTE: &[&str] = &[
    "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7",
    "#9c755f", "#bab0ac",
];

/// Fill colour for nodes that belong to no community.
const UNCLUSTERED_FILL: &str = "#cccccc";

// ── Public API ────────────────────────────────────────────────────────────────

/// Renders `graph` as a self-contained SVG document.
///
/// Output is always a single well-formed `<svg>…</svg>` root. Node positions are determined
/// solely by each node's index in `graph.nodes` (call [`Graph::sorted`] first for the canonical
/// R4 ordering). Nodes are placed on a circle whose radius scales with node count so adjacent
/// glyphs do not overlap. Edges are drawn as `<line>` elements beneath the node layer; an edge
/// whose source or target [`NodeId`] is absent from `graph.nodes` is silently skipped.
/// Self-loop edges (source == target in position space) are also skipped.
///
/// All node labels first use deterministic secret redaction and then pass through [`xml_escape`]
/// before embedding, neutralising XML-injection payloads such as
/// `</text><script>alert(1)</script>` (STRIDE-T). Redaction does not change node positions, edge
/// lines, or community-derived colours.
///
/// The function is infallible and never touches the filesystem, network, or system clock.
#[must_use]
pub fn render_svg(graph: &Graph) -> String {
    let n = graph.nodes.len();
    let (positions, side) = circular_layout(n);

    // `NodeId` → index in `positions` (equals index in `graph.nodes`).
    let id_to_idx: HashMap<NodeId, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id, i))
        .collect();

    // `NodeId` → CSS fill colour derived from community membership.
    let fill_map = build_fill_map(graph);

    // Pre-allocate: SVG header + style + per-node + per-edge + footer.
    let mut out = String::with_capacity(512_usize.saturating_add(n.saturating_mul(250)));

    // ── SVG header ────────────────────────────────────────────────────────────
    // `write!` on `String` is infallible (OOM is the only failure mode, which aborts).
    let _ = write!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         width=\"{side}\" height=\"{side}\" \
         viewBox=\"0 0 {side} {side}\">"
    );

    // ── Inline style ──────────────────────────────────────────────────────────
    out.push_str(
        "<style>\
         .hg-edge{stroke:#888888;stroke-width:1;stroke-opacity:0.55;}\
         .hg-node{stroke:#333333;stroke-width:1.5;}\
         .hg-label{font-family:sans-serif;font-size:11px;\
         text-anchor:middle;dominant-baseline:central;fill:#222222;\
         pointer-events:none;}\
         </style>",
    );

    // ── Edges (drawn first, behind nodes) ────────────────────────────────────
    for edge in &graph.edges {
        let Some(&si) = id_to_idx.get(&edge.source) else {
            continue;
        };
        let Some(&ti) = id_to_idx.get(&edge.target) else {
            continue;
        };
        let (x1, y1) = positions[si];
        let (x2, y2) = positions[ti];
        // Self-loops produce a zero-length line — invisible and uninformative; skip.
        if (x1 - x2).abs() < f64::EPSILON && (y1 - y2).abs() < f64::EPSILON {
            continue;
        }
        let _ = write!(
            out,
            "<line class=\"hg-edge\" x1=\"{x1:.2}\" y1=\"{y1:.2}\" \
             x2=\"{x2:.2}\" y2=\"{y2:.2}\"/>"
        );
    }

    // ── Nodes (circle glyph + text label) ────────────────────────────────────
    for (i, node) in graph.nodes.iter().enumerate() {
        let (cx, cy) = positions[i];
        let fill = fill_map.get(&node.id).copied().unwrap_or(UNCLUSTERED_FILL);
        // `xml_escape` converts `<`, `>`, `&`, `"`, `'` to entities and drops illegal XML
        // C0 controls — the full STRIDE-T guard for attacker-influenced labels.
        let redacted_label = redact_public_text(&node.label);
        let safe_label = xml_escape(&redacted_label);
        // Label text-anchor is centred horizontally; its baseline sits below the glyph.
        let ty = cy + NODE_RADIUS + LABEL_OFFSET;
        let _ = write!(
            out,
            "<circle class=\"hg-node\" cx=\"{cx:.2}\" cy=\"{cy:.2}\" \
             r=\"{NODE_RADIUS:.0}\" fill=\"{fill}\"/>\
             <text class=\"hg-label\" x=\"{cx:.2}\" y=\"{ty:.2}\">{safe_label}</text>"
        );
    }

    out.push_str("</svg>");
    out
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Computes a deterministic circular layout for `n` nodes.
///
/// Returns `(positions, canvas_side)` where `positions[i]` is the `(x, y)` centre of node `i`
/// in SVG user units, and `canvas_side` is the integer side-length of the square canvas.
///
/// Nodes start at the 12-o'clock position (`angle = -π/2`) and proceed clockwise. For `n == 0`
/// the position list is empty. For `n == 1` the single node is placed at the canvas centre.
///
/// The ring radius is large enough that adjacent node glyphs never overlap.
#[allow(
    clippy::cast_precision_loss,      // n, i: usize → f64; safe for n ≪ 2^53 (practical graphs)
    clippy::cast_possible_truncation, // canvas_f → u32; bounded by `.min(f64::from(u32::MAX))`
    clippy::cast_sign_loss            // canvas_f ≥ 0 by construction (all inputs positive)
)]
fn circular_layout(n: usize) -> (Vec<(f64, f64)>, u32) {
    if n == 0 {
        return (Vec::new(), MIN_CANVAS);
    }
    if n == 1 {
        let c = f64::from(MIN_CANVAS) / 2.0;
        return (vec![(c, c)], MIN_CANVAS);
    }
    // Minimum ring radius so adjacent node circles do not overlap.
    // Chord between adjacent nodes = 2·R·sin(π/n); we need chord ≥ 2·(NODE_RADIUS + gap).
    let gap = 6.0_f64;
    let min_r = (NODE_RADIUS + gap) / (PI / n as f64).sin();
    let ring_r = f64::max(120.0, min_r);
    // canvas_side = ceil(2·(ring_r + NODE_RADIUS + RING_PADDING)), clamped to u32 range.
    let canvas_f = ring_r
        .mul_add(2.0, (NODE_RADIUS + RING_PADDING) * 2.0)
        .ceil()
        .min(f64::from(u32::MAX));
    let side = (canvas_f as u32).max(MIN_CANVAS);
    let cx = f64::from(side) / 2.0;
    let cy = f64::from(side) / 2.0;
    let n_f = n as f64;
    let positions = (0..n)
        .map(|i| {
            // 12-o'clock start (−π/2), proceeding clockwise.
            let angle = (2.0 * PI).mul_add(i as f64 / n_f, -PI / 2.0);
            let x = ring_r.mul_add(angle.cos(), cx);
            let y = ring_r.mul_add(angle.sin(), cy);
            (x, y)
        })
        .collect();
    (positions, side)
}

/// Builds a `NodeId` → CSS fill-colour map from community membership.
///
/// The colour for community `c` is `PALETTE[c.id.get() % PALETTE.len()]`. Nodes absent from all
/// communities are not inserted; callers should fall back to [`UNCLUSTERED_FILL`].
fn build_fill_map(graph: &Graph) -> HashMap<NodeId, &'static str> {
    let mut map: HashMap<NodeId, &'static str> = HashMap::new();
    for community in &graph.communities {
        #[allow(clippy::cast_possible_truncation)]
        // u32 → usize: lossless on all Rust-supported targets (usize ≥ 32 bits)
        let idx = (community.id.get() as usize) % PALETTE.len();
        let fill = PALETTE[idx];
        for &member in &community.members {
            map.insert(member, fill);
        }
    }
    map
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use habitat_graph_core::{
        Community, CommunityId, Confidence, Edge, Graph, Manifest, Node, NodeId, Span,
    };

    use super::render_svg;

    // ── Fixtures ──────────────────────────────────────────────────────────────

    fn span() -> Span {
        Span::new(0, 1, 1, 1)
    }

    fn node(id: u32, label: &str, file: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: file.to_owned(),
            source_location: span(),
        }
    }

    fn edge(src: u32, tgt: u32, rel: &str) -> Edge {
        Edge {
            source: NodeId::new(src),
            target: NodeId::new(tgt),
            relation: rel.to_owned(),
            confidence: Confidence::Extracted,
        }
    }

    fn community(id: u32, members: &[u32]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: format!("c{id}"),
            members: members.iter().copied().map(NodeId::new).collect(),
        }
    }

    fn bare_graph() -> Graph {
        Graph {
            schema: "test".to_owned(),
            nodes: Vec::new(),
            node_content_ids: std::collections::BTreeMap::default(),
            edges: Vec::new(),
            communities: Vec::new(),
            manifest: Manifest {
                inputs: Vec::new(),
                tool_version: "test".to_owned(),
                generated_at: None,
            },
        }
    }

    fn count_tag(svg: &str, tag: &str) -> usize {
        svg.matches(tag).count()
    }

    // ── T01-T04: empty graph structure ───────────────────────────────────────

    #[test]
    fn t01_empty_graph_starts_with_svg_tag() {
        let svg = render_svg(&Graph::new());
        assert!(svg.starts_with("<svg"), "must start with <svg: {svg:.80}");
    }

    #[test]
    fn t02_empty_graph_ends_with_svg_close_tag() {
        let svg = render_svg(&Graph::new());
        assert!(svg.ends_with("</svg>"), "must end with </svg>: {svg:.80}");
    }

    #[test]
    fn t03_empty_graph_contains_xmlns_attribute() {
        let svg = render_svg(&Graph::new());
        assert!(
            svg.contains("xmlns=\"http://www.w3.org/2000/svg\""),
            "xmlns missing: {svg:.120}"
        );
    }

    #[test]
    fn t04_empty_graph_has_no_circle_element() {
        let svg = render_svg(&Graph::new());
        assert_eq!(
            count_tag(&svg, "<circle"),
            0,
            "empty graph must have 0 circles: {svg:.120}"
        );
    }

    // ── T05-T09: single-node graph ────────────────────────────────────────────

    #[test]
    fn t05_single_node_produces_one_circle() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Alpha", "a.rs"));
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<circle"),
            1,
            "expected 1 circle: {svg:.200}"
        );
    }

    #[test]
    fn t06_single_node_produces_one_text_element() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Alpha", "a.rs"));
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<text"), 1, "expected 1 text: {svg:.200}");
    }

    #[test]
    fn t07_single_node_no_lines_without_edges() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Solo", "s.rs"));
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<line"),
            0,
            "no edges → no lines: {svg:.200}"
        );
    }

    #[test]
    fn t08_single_node_label_appears_in_output() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "UniqueLabel", "a.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("UniqueLabel"),
            "label missing from svg: {svg:.200}"
        );
    }

    #[test]
    fn t09_single_node_canvas_is_at_least_min_canvas() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "X", "x.rs"));
        let svg = render_svg(&g);
        // The width/height attributes must reflect MIN_CANVAS = 300.
        assert!(
            svg.contains("width=\"300\""),
            "expected width=300: {svg:.200}"
        );
        assert!(
            svg.contains("height=\"300\""),
            "expected height=300: {svg:.200}"
        );
    }

    // ── T10-T13: node/edge count correspondence ───────────────────────────────

    #[test]
    fn t10_n_nodes_produce_n_circles() {
        let mut g = bare_graph();
        for i in 0..7_u32 {
            g.nodes.push(node(i, &format!("n{i}"), "f.rs"));
        }
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<circle"), 7, "expected 7 circles");
    }

    #[test]
    fn t11_two_nodes_one_edge_produces_one_line() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.edges.push(edge(1, 2, "calls"));
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<line"), 1, "expected 1 line: {svg:.300}");
    }

    #[test]
    fn t12_self_loop_edge_produces_no_line() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Recursive", "r.rs"));
        g.edges.push(edge(1, 1, "recurses")); // self-loop
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<line"),
            0,
            "self-loop must not produce a <line>: {svg:.300}"
        );
    }

    #[test]
    fn t13_edge_with_unknown_source_is_skipped() {
        let mut g = bare_graph();
        g.nodes.push(node(2, "Target", "t.rs"));
        g.edges.push(edge(99, 2, "mystery")); // NodeId 99 not in nodes
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<line"),
            0,
            "dangling-source edge must not emit <line>"
        );
    }

    // ── T14-T16: determinism (R4) ─────────────────────────────────────────────

    #[test]
    fn t14_same_graph_produces_identical_output() {
        let build = || {
            let mut g = bare_graph();
            g.nodes.push(node(1, "Alpha", "a.rs"));
            g.nodes.push(node(2, "Beta", "b.rs"));
            g.edges.push(edge(1, 2, "calls"));
            g.communities.push(community(0, &[1]));
            g
        };
        assert_eq!(
            render_svg(&build()),
            render_svg(&build()),
            "render_svg must be deterministic"
        );
    }

    #[test]
    fn t15_render_twice_on_same_instance_is_identical() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.edges.push(edge(1, 2, "e"));
        let first = render_svg(&g);
        let second = render_svg(&g);
        assert_eq!(
            first, second,
            "calling render_svg twice must yield identical strings"
        );
    }

    #[test]
    fn t16_node_order_determines_position() {
        // Same nodes, different order in graph.nodes → different SVG (different x1/y1 on edges).
        let build = |a_first: bool| {
            let mut g = bare_graph();
            let na = node(1, "Alpha", "a.rs");
            let nb = node(2, "Beta", "b.rs");
            if a_first {
                g.nodes.push(na);
                g.nodes.push(nb);
            } else {
                g.nodes.push(nb);
                g.nodes.push(na);
            }
            g.edges.push(edge(1, 2, "e"));
            g
        };
        // With a different ordering the edge endpoints swap, producing different x1/y1.
        assert_ne!(
            render_svg(&build(true)),
            render_svg(&build(false)),
            "different node orderings must produce different layouts"
        );
    }

    // ── T17-T24: XML injection / STRIDE-T ────────────────────────────────────

    #[test]
    fn t17_script_tag_injection_does_not_appear_raw() {
        let mut g = bare_graph();
        g.nodes
            .push(node(1, "</text><script>alert(1)</script>", "evil.rs"));
        let svg = render_svg(&g);
        assert!(
            !svg.contains("</text><script>"),
            "raw injection payload must not appear in output: {svg:.400}"
        );
    }

    #[test]
    fn t18_less_than_in_label_is_escaped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "a<b", "f.rs"));
        let svg = render_svg(&g);
        // After xml_escape: `a&lt;b`. The raw `<b` must not appear inside a text element.
        // We verify the entity appears and the raw `<b` does not follow `a`.
        assert!(
            svg.contains("a&lt;b"),
            "< must be escaped to &lt;: {svg:.300}"
        );
    }

    #[test]
    fn t19_greater_than_in_label_is_escaped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "a>b", "f.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("a&gt;b"),
            "> must be escaped to &gt;: {svg:.300}"
        );
    }

    #[test]
    fn t20_ampersand_in_label_is_escaped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "foo&bar", "f.rs"));
        let svg = render_svg(&g);
        assert!(svg.contains("foo&amp;bar"), "& must be escaped: {svg:.300}");
    }

    #[test]
    fn t21_double_quote_in_label_is_escaped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "say\"hi\"", "f.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("say&quot;hi&quot;"),
            "\" must be escaped: {svg:.300}"
        );
    }

    #[test]
    fn t22_single_quote_in_label_is_escaped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "it's", "f.rs"));
        let svg = render_svg(&g);
        assert!(svg.contains("it&apos;s"), "' must be escaped: {svg:.300}");
    }

    #[test]
    fn t23_null_char_dropped_in_label() {
        let mut g = bare_graph();
        // U+0000 is an illegal XML 1.0 character; xml_escape drops it.
        g.nodes.push(node(1, "ab\x00cd", "f.rs"));
        let svg = render_svg(&g);
        assert!(
            !svg.contains('\x00'),
            "null char must not appear in output: {svg:.300}"
        );
        assert!(
            svg.contains("abcd"),
            "surrounding chars must survive: {svg:.300}"
        );
    }

    #[test]
    fn t24_malicious_label_no_structural_breakout() {
        // The full closing-tag injection: if xml_escape works, no `<script>` tag can break out.
        let mut g = bare_graph();
        g.nodes
            .push(node(1, "</text></svg><script>pwned</script>", "evil.rs"));
        let svg = render_svg(&g);
        // The SVG must still end with a single `</svg>`.
        assert!(
            svg.ends_with("</svg>"),
            "SVG must close correctly even with hostile label"
        );
        assert!(
            !svg.contains("<script>"),
            "raw <script> tag must not appear"
        );
    }

    // ── T25-T32: SVG structural properties ───────────────────────────────────

    #[test]
    fn t25_output_has_single_svg_root() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<svg"),
            1,
            "must have exactly one <svg open tag"
        );
        assert_eq!(
            count_tag(&svg, "</svg>"),
            1,
            "must have exactly one </svg> close tag"
        );
    }

    #[test]
    fn t26_output_contains_style_element() {
        let svg = render_svg(&Graph::new());
        assert!(svg.contains("<style>"), "must contain inline <style>");
        assert!(svg.contains("</style>"), "must close </style>");
    }

    #[test]
    fn t27_circle_has_r_attribute() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "N", "n.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains(" r=\"18\""),
            "circle must have r=\"18\": {svg:.300}"
        );
    }

    #[test]
    fn t28_circle_has_fill_attribute() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "N", "n.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\""),
            "circle must have fill attribute: {svg:.300}"
        );
    }

    #[test]
    fn t29_line_has_x1_y1_x2_y2_attributes() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.edges.push(edge(1, 2, "calls"));
        let svg = render_svg(&g);
        assert!(svg.contains("x1=\""), "line must have x1: {svg:.400}");
        assert!(svg.contains("y1=\""), "line must have y1: {svg:.400}");
        assert!(svg.contains("x2=\""), "line must have x2: {svg:.400}");
        assert!(svg.contains("y2=\""), "line must have y2: {svg:.400}");
    }

    #[test]
    fn t30_text_elements_carry_hg_label_class() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "LabelNode", "l.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("class=\"hg-label\""),
            "text must have hg-label class: {svg:.300}"
        );
    }

    #[test]
    fn t31_viewbox_attribute_present() {
        let svg = render_svg(&Graph::new());
        assert!(
            svg.contains("viewBox=\""),
            "must have viewBox attribute: {svg:.200}"
        );
    }

    #[test]
    fn t32_width_and_height_attributes_present() {
        let svg = render_svg(&Graph::new());
        assert!(svg.contains("width=\""), "must have width attribute");
        assert!(svg.contains("height=\""), "must have height attribute");
    }

    // ── T33-T39: layout properties ────────────────────────────────────────────

    #[test]
    fn t33_empty_graph_canvas_is_min_canvas() {
        let svg = render_svg(&Graph::new());
        // MIN_CANVAS = 300.
        assert!(
            svg.contains("width=\"300\""),
            "empty graph width must be 300: {svg:.200}"
        );
        assert!(
            svg.contains("height=\"300\""),
            "empty graph height must be 300: {svg:.200}"
        );
    }

    #[test]
    fn t34_two_nodes_have_distinct_positions() {
        // For 2 nodes on a circle they land at 12-o'clock (angle = -π/2) and 6-o'clock
        // (angle = π/2), giving the same cx but distinct cy values.  We prove distinct
        // positions by verifying the edge line's y1 ≠ y2.
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.edges.push(edge(1, 2, "e")); // need an edge to read its endpoints
        let svg = render_svg(&g);
        let line_pos = svg.find("<line").expect("line element must be present");
        let frag = &svg[line_pos..];
        let y1_pos = frag.find("y1=\"").expect("y1 attribute") + 4;
        let y1_end = frag[y1_pos..].find('"').expect("y1 close quote") + y1_pos;
        let y2_pos = frag.find("y2=\"").expect("y2 attribute") + 4;
        let y2_end = frag[y2_pos..].find('"').expect("y2 close quote") + y2_pos;
        assert_ne!(
            &frag[y1_pos..y1_end],
            &frag[y2_pos..y2_end],
            "two nodes must have distinct y positions: y1={} y2={}",
            &frag[y1_pos..y1_end],
            &frag[y2_pos..y2_end]
        );
    }

    #[test]
    fn t35_position_count_matches_node_count() {
        for count in [0_usize, 1, 3, 8, 12] {
            let mut g = bare_graph();
            let up_to = u32::try_from(count).expect("test count fits in u32");
            for i in 0..up_to {
                g.nodes.push(node(i, &format!("n{i}"), "f.rs"));
            }
            let svg = render_svg(&g);
            assert_eq!(
                count_tag(&svg, "<circle"),
                count,
                "count={count}: circle count must match node count"
            );
        }
    }

    #[test]
    fn t36_canvas_grows_with_many_nodes() {
        // 50 nodes → canvas wider than 300.
        let mut g = bare_graph();
        for i in 0..50_u32 {
            g.nodes.push(node(i, &format!("node{i}"), "f.rs"));
        }
        let svg = render_svg(&g);
        // Width is a decimal string in the attribute; extract and parse it.
        let w_start = svg.find("width=\"").expect("width missing") + 7;
        let w_end = svg[w_start..].find('"').expect("width close quote") + w_start;
        let width: u32 = svg[w_start..w_end].parse().expect("width is u32");
        assert!(
            width > 300,
            "50-node canvas must be wider than 300 px; got {width}"
        );
    }

    #[test]
    fn t37_one_node_placed_at_canvas_center() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Center", "c.rs"));
        let svg = render_svg(&g);
        // MIN_CANVAS=300, center=150.0. Circle cx should be "150.00".
        assert!(
            svg.contains("cx=\"150.00\""),
            "single node must be at center (cx=150.00): {svg:.300}"
        );
        assert!(
            svg.contains("cy=\"150.00\""),
            "single node must be at center (cy=150.00): {svg:.300}"
        );
    }

    #[test]
    fn t38_six_nodes_produce_six_text_elements() {
        let mut g = bare_graph();
        for i in 0..6_u32 {
            g.nodes.push(node(i, &format!("lbl{i}"), "f.rs"));
        }
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<text"), 6, "must have one <text per node");
    }

    // ── T40-T47: community colours ────────────────────────────────────────────

    #[test]
    fn t40_unclustered_node_gets_unclustered_fill() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Lonely", "l.rs"));
        // No communities added.
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\"#cccccc\""),
            "unclustered node must use UNCLUSTERED_FILL (#cccccc): {svg:.300}"
        );
    }

    #[test]
    fn t41_clustered_node_gets_palette_fill_not_default() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Member", "m.rs"));
        g.communities.push(community(0, &[1])); // community 0 → PALETTE[0] = "#4e79a7"
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\"#4e79a7\""),
            "community-0 node must use PALETTE[0] (#4e79a7): {svg:.300}"
        );
        assert!(
            !svg.contains("fill=\"#cccccc\""),
            "clustered node must not use UNCLUSTERED_FILL: {svg:.300}"
        );
    }

    #[test]
    fn t42_community_zero_uses_palette_index_zero() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "C0Node", "c.rs"));
        g.communities.push(community(0, &[1]));
        let svg = render_svg(&g);
        // PALETTE[0] = "#4e79a7"
        assert!(
            svg.contains("fill=\"#4e79a7\""),
            "community 0 must use palette[0]: {svg:.300}"
        );
    }

    #[test]
    fn t43_community_id_wraps_palette_length() {
        // PALETTE has 10 entries; community id 10 wraps to index 0 → same colour as community 0.
        let mut g = bare_graph();
        g.nodes.push(node(1, "Wrapped", "w.rs"));
        g.communities.push(community(10, &[1])); // 10 % 10 = 0 → PALETTE[0] = "#4e79a7"
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\"#4e79a7\""),
            "community 10 must wrap to palette[0]: {svg:.300}"
        );
    }

    #[test]
    fn t44_two_communities_produce_different_fills() {
        // community 0 → PALETTE[0]="#4e79a7"; community 1 → PALETTE[1]="#f28e2b".
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.communities.push(community(0, &[1]));
        g.communities.push(community(1, &[2]));
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\"#4e79a7\""),
            "community-0 fill missing: {svg:.400}"
        );
        assert!(
            svg.contains("fill=\"#f28e2b\""),
            "community-1 fill missing: {svg:.400}"
        );
    }

    #[test]
    fn t45_community_one_uses_palette_index_one() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "N", "n.rs"));
        g.communities.push(community(1, &[1])); // PALETTE[1] = "#f28e2b"
        let svg = render_svg(&g);
        assert!(
            svg.contains("fill=\"#f28e2b\""),
            "community 1 must use palette[1]: {svg:.300}"
        );
    }

    #[test]
    fn t46_fill_attribute_is_hex_color_string() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "X", "x.rs"));
        let svg = render_svg(&g);
        // Both UNCLUSTERED_FILL and PALETTE entries start with `#`.
        assert!(
            svg.contains("fill=\"#"),
            "fill must be a hex color: {svg:.300}"
        );
    }

    // ── T47-T53: edge cases ───────────────────────────────────────────────────

    #[test]
    fn t47_twenty_node_graph_produces_valid_svg_shape() {
        let mut g = bare_graph();
        for i in 0..20_u32 {
            g.nodes.push(node(i, &format!("node{i}"), "f.rs"));
        }
        for i in 0..19_u32 {
            g.edges.push(edge(i, i + 1, "chain"));
        }
        let svg = render_svg(&g);
        assert!(svg.starts_with("<svg"), "20-node svg must start with <svg");
        assert!(svg.ends_with("</svg>"), "20-node svg must end with </svg>");
        assert_eq!(count_tag(&svg, "<circle"), 20, "must have 20 circles");
        // 19 non-self-loop edges.
        assert_eq!(count_tag(&svg, "<line"), 19, "must have 19 lines");
    }

    #[test]
    fn t48_edge_with_missing_target_is_skipped() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Src", "s.rs"));
        g.edges.push(edge(1, 999, "orphan")); // target NodeId 999 not in nodes
        let svg = render_svg(&g);
        assert_eq!(
            count_tag(&svg, "<line"),
            0,
            "dangling-target edge must not emit <line>"
        );
    }

    #[test]
    fn t49_graph_with_no_edges_has_no_lines() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.nodes.push(node(3, "C", "c.rs"));
        // No edges.
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<line"), 0, "no edges → no lines");
    }

    #[test]
    fn t50_all_xml_metachar_label_is_fully_escaped() {
        let mut g = bare_graph();
        // All five XML metacharacters.
        g.nodes.push(node(1, "&<>\"'", "f.rs"));
        let svg = render_svg(&g);
        // None of the raw metacharacters may appear inside a text element.
        // We verify by checking the escaped form appears.
        assert!(svg.contains("&amp;"), "& must escape to &amp;: {svg:.400}");
        assert!(svg.contains("&lt;"), "< must escape to &lt;: {svg:.400}");
        assert!(svg.contains("&gt;"), "> must escape to &gt;: {svg:.400}");
        assert!(
            svg.contains("&quot;"),
            "\" must escape to &quot;: {svg:.400}"
        );
        assert!(
            svg.contains("&apos;"),
            "' must escape to &apos;: {svg:.400}"
        );
    }

    #[test]
    fn t51_unicode_label_passes_through_xml_escape() {
        // Valid Unicode that doesn't need escaping must pass unchanged.
        let mut g = bare_graph();
        g.nodes.push(node(1, "café — Ω", "f.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("café — Ω"),
            "unicode chars must survive: {svg:.300}"
        );
    }

    #[test]
    fn t52_multiple_edges_all_produce_lines() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.nodes.push(node(3, "C", "c.rs"));
        g.edges.push(edge(1, 2, "calls"));
        g.edges.push(edge(2, 3, "imports"));
        g.edges.push(edge(1, 3, "defines"));
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<line"), 3, "3 edges → 3 lines: {svg:.400}");
    }

    #[test]
    fn t53_hg_node_class_appears_on_circles() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "Classed", "c.rs"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("class=\"hg-node\""),
            "circles must carry class=hg-node: {svg:.300}"
        );
    }

    #[test]
    fn t54_hg_edge_class_appears_on_lines() {
        let mut g = bare_graph();
        g.nodes.push(node(1, "A", "a.rs"));
        g.nodes.push(node(2, "B", "b.rs"));
        g.edges.push(edge(1, 2, "e"));
        let svg = render_svg(&g);
        assert!(
            svg.contains("class=\"hg-edge\""),
            "lines must carry class=hg-edge: {svg:.400}"
        );
    }

    #[test]
    fn t55_secret_pattern_label_is_redacted_without_topology_change() {
        let mut g = bare_graph();
        g.nodes
            .push(node(77, "api_key_assignment_refused", "privacy.rs"));
        let svg = render_svg(&g);
        assert_eq!(count_tag(&svg, "<circle"), 1);
        assert!(!svg.contains("api_key_assignment_refused"));
        assert!(svg.contains("[REDACTED:api_key]"));
    }

    #[test]
    fn t56_sorted_graph_output_is_deterministic() {
        let build = || {
            let mut g = bare_graph();
            g.nodes.push(node(3, "Gamma", "c.rs"));
            g.nodes.push(node(1, "Alpha", "a.rs"));
            g.nodes.push(node(2, "Beta", "b.rs"));
            g.edges.push(edge(1, 2, "calls"));
            g.edges.push(edge(2, 3, "imports"));
            g.communities.push(community(0, &[1, 2]));
            g.communities.push(community(1, &[3]));
            g.sorted()
        };
        let out_a = render_svg(&build());
        let out_b = render_svg(&build());
        assert_eq!(
            out_a, out_b,
            "sorted graph must produce identical SVG each time"
        );
    }
}
