//! Self-contained interactive HTML viewer (the graphify `graph.html` analogue).
//!
//! [`render_html`] emits a **single, dependency-free** `.html`: the graph's node-link JSON is embedded
//! in the page and rendered by a small vanilla-canvas force-directed viewer — community-coloured nodes,
//! pan/zoom, label search, and click-to-inspect. No CDN, no bundled framework: the file opens offline in
//! any browser. (For very large graphs a `sigma`/`cytoscape` exporter is a future refinement; this viewer
//! targets the typical per-service extract.)
//!
//! Security: the embedded JSON has every `<` escaped to `<` so a label can never break out of the
//! `<script type="application/json">` island; node labels are already render-safe (the export path runs
//! them through [`display_safe`](habitat_graph_core::display_safe)).

use crate::json::to_node_link;
use habitat_graph_core::{display_safe, Graph, Result};

/// The viewer shell. `__TITLE__` and `__GRAPH_DATA__` are substituted (not `format!` — the template is
/// full of literal `{}`); the data island is parsed by the embedded script.
const VIEWER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>__TITLE__</title>
<style>
 html,body{margin:0;height:100%;background:#0b0f14;color:#cdd9e5;font:13px system-ui,sans-serif;overflow:hidden}
 #c{display:block;position:absolute;inset:0;cursor:grab}
 #hud{position:absolute;top:10px;left:10px;z-index:2;background:rgba(15,22,30,.88);padding:10px 12px;border-radius:8px;border:1px solid #1d2a38;max-width:300px}
 #hud h1{font-size:13px;margin:0 0 8px;color:#7ee787;font-weight:600}
 #search{width:100%;box-sizing:border-box;background:#0b1117;border:1px solid #25323f;color:#cdd9e5;padding:6px 8px;border-radius:6px;outline:none}
 #search:focus{border-color:#3b5168}
 #stat{margin-top:6px}
 #info{position:absolute;top:10px;right:10px;z-index:2;background:rgba(15,22,30,.92);padding:10px 12px;border-radius:8px;border:1px solid #1d2a38;max-width:320px;display:none}
 .muted{color:#6b7a8d}
 #hint{position:absolute;bottom:8px;left:10px;color:#5261733;z-index:2}
</style></head>
<body>
<canvas id="c"></canvas>
<div id="hud"><h1>__TITLE__</h1><input id="search" placeholder="search nodes…" autocomplete="off"><div id="stat" class="muted"></div></div>
<div id="info"></div>
<div id="hint" class="muted">drag = pan · wheel = zoom · click a node = inspect · type to search</div>
<script type="application/json" id="graph-data">__GRAPH_DATA__</script>
<script>
(function(){
 var raw=JSON.parse(document.getElementById('graph-data').textContent);
 var cv=document.getElementById('c'),ctx=cv.getContext('2d'),W,H;
 function resize(){W=cv.width=innerWidth;H=cv.height=innerHeight;} resize(); addEventListener('resize',resize);
 var idOf=new Map();
 var N=raw.nodes.map(function(n,i){idOf.set(n.id,i);return{id:n.id,label:n.label||('#'+n.id),file:n.source_file||'',loc:n.source_location||'',com:(n.community|0),x:(Math.random()-0.5)*600,y:(Math.random()-0.5)*600,vx:0,vy:0,deg:0};});
 var L=raw.links.map(function(l){return{s:idOf.get(l.source),t:idOf.get(l.target),rel:l.relation||''};}).filter(function(l){return l.s!=null&&l.t!=null;});
 L.forEach(function(l){N[l.s].deg++;N[l.t].deg++;});
 function color(c){return 'hsl('+((c*137.508)%360)+' 65% 60%)';}
 var scale=1,ox=W/2,oy=H/2,sel=null,hi=new Set(),alpha=1;
 function tick(){
  if(alpha>0.004){
   var k=0.02,rep=1400,i,j;
   for(i=0;i<N.length;i++){var a=N[i];
    for(j=i+1;j<N.length;j++){var b=N[j];var dx=a.x-b.x,dy=a.y-b.y,d2=dx*dx+dy*dy+0.01,d=Math.sqrt(d2),f=rep/d2;a.vx+=f*dx/d;a.vy+=f*dy/d;b.vx-=f*dx/d;b.vy-=f*dy/d;}
    a.vx-=a.x*0.0009;a.vy-=a.y*0.0009;}
   for(i=0;i<L.length;i++){var p=N[L[i].s],q=N[L[i].t];var ex=q.x-p.x,ey=q.y-p.y;p.vx+=ex*k;p.vy+=ey*k;q.vx-=ex*k;q.vy-=ey*k;}
   for(i=0;i<N.length;i++){var nn=N[i];nn.x+=nn.vx*alpha;nn.y+=nn.vy*alpha;nn.vx*=0.85;nn.vy*=0.85;}
   alpha*=0.992;
  }
  draw(); requestAnimationFrame(tick);
 }
 function draw(){
  ctx.setTransform(1,0,0,1,0,0);ctx.clearRect(0,0,W,H);ctx.setTransform(scale,0,0,scale,ox,oy);
  ctx.lineWidth=0.6/scale;ctx.strokeStyle='rgba(120,140,160,0.12)';ctx.beginPath();
  for(var i=0;i<L.length;i++){var a=N[L[i].s],b=N[L[i].t];ctx.moveTo(a.x,a.y);ctx.lineTo(b.x,b.y);} ctx.stroke();
  for(i=0;i<N.length;i++){var n=N[i],r=2+Math.min(7,n.deg),on=hi.size?hi.has(n):true;
   ctx.globalAlpha=on?1:0.1;ctx.beginPath();ctx.arc(n.x,n.y,r,0,6.2832);ctx.fillStyle=color(n.com);ctx.fill();
   if(n===sel){ctx.lineWidth=2/scale;ctx.strokeStyle='#fff';ctx.stroke();}}
  ctx.globalAlpha=1;
  if(scale>1.7){ctx.fillStyle='#9fb3c8';ctx.font=(10/scale)+'px sans-serif';for(i=0;i<N.length;i++){var m=N[i];if(hi.size&&!hi.has(m))continue;ctx.fillText(m.label,m.x+5,m.y+3);}}
 }
 var drag=null;
 cv.addEventListener('mousedown',function(e){drag={x:e.clientX,y:e.clientY,ox:ox,oy:oy,moved:false};});
 addEventListener('mousemove',function(e){if(!drag)return;ox=drag.ox+(e.clientX-drag.x);oy=drag.oy+(e.clientY-drag.y);drag.moved=true;});
 addEventListener('mouseup',function(e){if(drag&&!drag.moved)pick(e);drag=null;});
 cv.addEventListener('wheel',function(e){e.preventDefault();var f=e.deltaY<0?1.1:0.9,mx=(e.clientX-ox)/scale,my=(e.clientY-oy)/scale;scale*=f;ox=e.clientX-mx*scale;oy=e.clientY-my*scale;},{passive:false});
 function pick(e){var mx=(e.clientX-ox)/scale,my=(e.clientY-oy)/scale,best=null,bd=1e9;
  for(var i=0;i<N.length;i++){var n=N[i],dx=n.x-mx,dy=n.y-my,d=dx*dx+dy*dy;if(d<bd){bd=d;best=n;}}
  var info=document.getElementById('info');
  if(best&&bd<400/scale){sel=best;info.style.display='block';info.innerHTML='<b>'+esc(best.label)+'</b><br><span class=muted>'+esc(best.file)+' '+esc(best.loc)+'</span><br>community '+best.com+' · degree '+best.deg;}
  else{sel=null;info.style.display='none';}
 }
 function esc(s){return String(s).replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c];});}
 var search=document.getElementById('search');
 search.addEventListener('input',function(){var q=search.value.toLowerCase().trim();hi=new Set();if(q){for(var i=0;i<N.length;i++)if(N[i].label.toLowerCase().indexOf(q)>=0)hi.add(N[i]);}});
 document.getElementById('stat').textContent=N.length+' nodes · '+L.length+' edges · '+(new Set(N.map(function(n){return n.com;})).size)+' communities';
 tick();
})();
</script></body></html>"#;

/// Renders a self-contained interactive `graph.html` for `graph`.
///
/// The page embeds the node-link JSON (graphify-compatible) and a dependency-free canvas viewer.
///
/// # Errors
/// Returns [`GraphError::Schema`](habitat_graph_core::GraphError::Schema) if the graph cannot be
/// serialized to node-link JSON.
pub fn render_html(graph: &Graph) -> Result<String> {
    let json = to_node_link(graph)?;
    // Prevent `</script>` breakout from any label: escape `<` (JSON has no structural `<`).
    let safe_json = json.replace('<', "\\u003c");
    let (nodes, edges, communities) = graph.counts();
    let title = display_safe(&format!(
        "habitat-graph — {nodes} nodes / {edges} edges / {communities} communities"
    ));
    Ok(VIEWER
        .replace("__TITLE__", &title)
        .replace("__GRAPH_DATA__", &safe_json))
}

#[cfg(test)]
mod tests {
    use super::render_html;
    use habitat_graph_core::{Confidence, Edge, Graph, Node, NodeId, Span};

    fn node(id: u32, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            label: label.to_owned(),
            source_file: "src/lib.rs".to_owned(),
            source_location: Span::new(0, 1, 1, 1),
        }
    }
    fn sample() -> Graph {
        let mut g = Graph::new();
        g.nodes = vec![node(1, "Alpha"), node(2, "Beta")];
        g.edges = vec![Edge {
            source: NodeId::new(1),
            target: NodeId::new(2),
            relation: "calls".to_owned(),
            confidence: Confidence::Extracted,
        }];
        g
    }

    /// Extract the embedded JSON island and un-escape the `<` guard back to valid JSON.
    fn embedded_json(html: &str) -> String {
        let start = html.find("id=\"graph-data\">").expect("data tag") + "id=\"graph-data\">".len();
        let end = html[start..].find("</script>").expect("close") + start;
        html[start..end].replace("\\u003c", "<")
    }

    #[test]
    fn is_a_full_html_document() {
        let h = render_html(&sample()).expect("ok");
        assert!(h.starts_with("<!doctype html>"));
        assert!(h.contains("<canvas id=\"c\">"));
        assert!(h.trim_end().ends_with("</html>"));
    }

    #[test]
    fn carries_a_search_box_and_info_panel() {
        let h = render_html(&sample()).expect("ok");
        assert!(h.contains("id=\"search\""));
        assert!(h.contains("id=\"info\""));
    }

    #[test]
    fn title_reflects_counts() {
        let h = render_html(&sample()).expect("ok");
        assert!(
            h.contains("2 nodes / 1 edges / 0 communities"),
            "{}",
            &h[..200]
        );
    }

    #[test]
    fn embeds_the_node_labels() {
        let h = render_html(&sample()).expect("ok");
        let data = embedded_json(&h);
        assert!(data.contains("Alpha") && data.contains("Beta"));
    }

    #[test]
    fn embedded_data_is_valid_parseable_json() {
        let h = render_html(&sample()).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("valid json");
        assert_eq!(v["nodes"].as_array().expect("nodes").len(), 2);
        assert_eq!(v["links"].as_array().expect("links").len(), 1);
    }

    #[test]
    fn embedded_data_keeps_node_link_keys() {
        let h = render_html(&sample()).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("json");
        for key in ["nodes", "links", "directed", "multigraph"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn script_breakout_is_neutralized() {
        // A label containing `</script>` must NOT appear raw in the data island.
        let mut g = Graph::new();
        g.nodes = vec![node(1, "evil</script><img src=x>")];
        let h = render_html(&g).expect("ok");
        let data_region = {
            let s = h.find("id=\"graph-data\">").expect("tag") + "id=\"graph-data\">".len();
            let e = h[s..].find("</script>").expect("close") + s;
            &h[s..e]
        };
        assert!(
            !data_region.contains("</script>"),
            "raw </script> leaked into data island"
        );
        // …but it still round-trips to the original label after un-escaping.
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("json");
        assert_eq!(v["nodes"][0]["label"], "evil</script><img src=x>");
    }

    #[test]
    fn embedded_json_uses_the_shared_secret_redaction_policy() {
        let mut g = Graph::new();
        g.nodes = vec![node(1, "api_key_assignment_refused")];
        let h = render_html(&g).expect("ok");
        let data = embedded_json(&h);
        assert!(!data.contains("api_key_assignment_refused"));
        let v: serde_json::Value = serde_json::from_str(&data).expect("json");
        assert_eq!(v["nodes"][0]["id"], 1);
        assert_eq!(v["nodes"][0]["label"], "[REDACTED:api_key]");
    }

    #[test]
    fn empty_graph_renders_without_panic() {
        let h = render_html(&Graph::new()).expect("ok");
        assert!(h.contains("0 nodes / 0 edges / 0 communities"));
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("json");
        assert_eq!(v["nodes"].as_array().expect("nodes").len(), 0);
    }

    #[test]
    fn is_self_contained_no_external_urls() {
        let h = render_html(&sample()).expect("ok");
        assert!(
            !h.contains("http://") && !h.contains("https://"),
            "viewer must be offline-self-contained"
        );
        assert!(!h.contains("src=\"http"));
    }

    #[test]
    fn viewer_script_has_force_sim_and_interactions() {
        let h = render_html(&sample()).expect("ok");
        assert!(h.contains("requestAnimationFrame"));
        assert!(h.contains("wheel"));
        assert!(h.contains("getContext('2d')"));
    }

    #[test]
    fn community_is_embedded_for_colouring() {
        let mut g = sample();
        g.nodes[0].label = "Gamma".to_owned();
        let h = render_html(&g).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("json");
        assert!(v["nodes"][0].get("community").is_some());
    }

    #[test]
    fn larger_graph_embeds_all_nodes() {
        let mut g = Graph::new();
        for i in 0..200u32 {
            g.nodes.push(node(i + 1, &format!("n{i}")));
        }
        let h = render_html(&g).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&embedded_json(&h)).expect("json");
        assert_eq!(v["nodes"].as_array().expect("nodes").len(), 200);
    }
}
