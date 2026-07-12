//! The `extract` command — the full pipeline (detect → extract → build → analyze → export → write).

use std::collections::HashSet;
use std::path::{Component, Path};

use habitat_graph_core::{Graph, GraphError, Result};

/// Ownership manifest for generated Obsidian notes. Only files recorded here (or recognized by
/// the conservative legacy signature during first migration) may be removed on a later sync.
const VAULT_MANIFEST: &str = ".habitat-graph-generated.json";
const VAULT_MANIFEST_SCHEMA: &str = "habitat-graph.vault-manifest.v1";
const WIKI_MANIFEST: &str = ".habitat-graph-generated.json";
const WIKI_MANIFEST_SCHEMA: &str = "habitat-graph.wiki-manifest.v1";

/// Optional PB exporter artifacts to emit alongside the always-written core artifacts.
///
/// All default to `false` (F13: human-facing exporters never burden the agent-critical path —
/// `graph.json` + `GRAPH_REPORT.md` + `graph.html` are always written; these are opt-in).
// A flat set of independent on/off CLI toggles is the natural representation here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtractOpts {
    /// Emit `graph.svg` (a deterministically laid-out drawing).
    pub svg: bool,
    /// Emit `graph.graphml` (Gephi/yEd import).
    pub graphml: bool,
    /// Emit `graph.cypher` (Neo4j import script).
    pub neo4j: bool,
    /// Emit a `wiki/` directory (one Markdown article per node + `index.md`).
    pub wiki: bool,
}

/// Runs the extraction pipeline over `dir` and writes the core artifacts (`graph.json` +
/// `GRAPH_REPORT.md` + `graph.html`) into `out`, with default (no extra) exporters.
///
/// Returns a process exit code: `0` on success, `4` on any error (diagnostics to stderr).
#[must_use]
pub fn run(dir: &Path, out: &Path, vault: Option<&Path>) -> u8 {
    run_artifacts(dir, out, vault, ExtractOpts::default())
}

/// Like [`run`], but additionally emits the opt-in PB exporter artifacts selected in `opts`
/// (`--svg`/`--graphml`/`--neo4j`/`--wiki`).
///
/// On success, prints a one-line summary plus a line per extra artifact written.
#[must_use]
pub fn run_artifacts(dir: &Path, out: &Path, vault: Option<&Path>, opts: ExtractOpts) -> u8 {
    match run_inner(dir, out, vault, opts) {
        Ok((n, e, c)) => {
            println!(
                "graph: {n} nodes, {e} edges, {c} communities -> {}",
                out.display()
            );
            if let Some(v) = vault {
                println!("obsidian vault ({n} notes + _MOC) -> {}", v.display());
            }
            if opts.svg {
                println!("svg -> {}", out.join("graph.svg").display());
            }
            if opts.graphml {
                println!("graphml -> {}", out.join("graph.graphml").display());
            }
            if opts.neo4j {
                println!("cypher -> {}", out.join("graph.cypher").display());
            }
            if opts.wiki {
                println!(
                    "wiki ({n} articles + index) -> {}",
                    out.join("wiki").display()
                );
            }
            0
        }
        Err(err) => {
            eprintln!("error: {err}");
            4
        }
    }
}

/// Returns whether `filename` is a safe single-component generated Markdown filename.
fn safe_vault_filename(filename: &str) -> bool {
    let path = Path::new(filename);
    path.extension().is_some_and(|extension| extension == "md")
        && path.components().count() == 1
        && matches!(path.components().next(), Some(Component::Normal(_)))
        && !filename.contains('/')
        && !filename.contains('\\')
}

/// Recognizes the exact frontmatter signature emitted by legacy habitat-graph node notes.
///
/// This migration path is deliberately conservative: it requires the ordered generated keys,
/// numeric id/line/degree fields, the `hg/node` ownership tag, and a generated heading. Arbitrary
/// user Markdown is never treated as generated merely because its filename resembles a label.
fn is_legacy_generated_node_note(content: &str) -> bool {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return false;
    }
    let Some(id) = lines.next().and_then(|line| line.strip_prefix("id: ")) else {
        return false;
    };
    if id.parse::<u32>().is_err() {
        return false;
    }

    let frontmatter: Vec<&str> = lines.by_ref().take_while(|line| *line != "---").collect();
    let has_ordered_fields = ["crate: ", "lang: ", "file: \"", "line: ", "degree: "]
        .iter()
        .all(|prefix| frontmatter.iter().any(|line| line.starts_with(prefix)));
    let has_owner_tag = frontmatter
        .iter()
        .any(|line| line.starts_with("tags: [hg/node"));
    let has_heading = lines.any(|line| line.starts_with("# "));
    has_ordered_fields && has_owner_tag && has_heading
}

/// Loads the generated-file ownership set, migrating conservative legacy note signatures when no
/// manifest exists. An invalid existing manifest fails closed rather than guessing ownership.
fn generated_vault_ownership(vault_dir: &Path) -> Result<HashSet<String>> {
    let manifest_path = vault_dir.join(VAULT_MANIFEST);
    if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|error| GraphError::Io(format!("vault manifest read: {error}")))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| GraphError::Schema(format!("vault manifest parse: {error}")))?;
        if value["schema"] != VAULT_MANIFEST_SCHEMA {
            return Err(GraphError::Schema(format!(
                "unsupported vault manifest schema: {:?}",
                value["schema"]
            )));
        }
        let files = value["files"].as_array().ok_or_else(|| {
            GraphError::Schema("vault manifest `files` must be an array".to_owned())
        })?;
        return files
            .iter()
            .map(|entry| {
                let filename = entry.as_str().ok_or_else(|| {
                    GraphError::Schema("vault manifest filename must be a string".to_owned())
                })?;
                if !safe_vault_filename(filename) {
                    return Err(GraphError::Guard(format!(
                        "unsafe generated vault filename in manifest: {filename:?}"
                    )));
                }
                Ok(filename.to_owned())
            })
            .collect();
    }

    let mut owned = HashSet::new();
    for entry in std::fs::read_dir(vault_dir)
        .map_err(|error| GraphError::Io(format!("vault inventory: {error}")))?
    {
        let entry = entry.map_err(|error| GraphError::Io(format!("vault entry: {error}")))?;
        if !entry
            .file_type()
            .map_err(|error| GraphError::Io(format!("vault file type: {error}")))?
            .is_file()
        {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().into_owned();
        if !safe_vault_filename(&filename) {
            continue;
        }
        let content = std::fs::read_to_string(entry.path())
            .map_err(|error| GraphError::Io(format!("legacy vault note {filename}: {error}")))?;
        if is_legacy_generated_node_note(&content) {
            owned.insert(filename);
        }
    }
    Ok(owned)
}

fn write_vault_manifest(vault_dir: &Path, names: &HashSet<String>) -> Result<()> {
    let mut files: Vec<&str> = names.iter().map(String::as_str).collect();
    files.sort_unstable();
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "schema": VAULT_MANIFEST_SCHEMA,
        "files": files,
    }))
    .map_err(|error| GraphError::Schema(format!("vault manifest serialize: {error}")))?;
    let temporary = vault_dir.join(format!(".{VAULT_MANIFEST}.tmp.{}", std::process::id()));
    std::fs::write(&temporary, manifest.as_bytes())
        .map_err(|error| GraphError::Io(format!("vault manifest temp write: {error}")))?;
    std::fs::rename(&temporary, vault_dir.join(VAULT_MANIFEST))
        .map_err(|error| GraphError::Io(format!("vault manifest rename: {error}")))
}

/// Synchronizes generated notes exactly while preserving every unowned/user-authored file.
fn sync_generated_vault(vault_dir: &Path, rendered: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(vault_dir).map_err(|error| GraphError::Io(error.to_string()))?;
    let prior_owned = generated_vault_ownership(vault_dir)?;
    let current_names: HashSet<String> = rendered
        .iter()
        .map(|(filename, _)| filename.clone())
        .collect();

    for filename in &current_names {
        if !safe_vault_filename(filename) {
            return Err(GraphError::Guard(format!(
                "exporter produced unsafe vault filename: {filename:?}"
            )));
        }
        let destination = vault_dir.join(filename);
        if destination.exists() && !prior_owned.contains(filename) {
            return Err(GraphError::Guard(format!(
                "refusing to overwrite unowned vault file: {}",
                destination.display()
            )));
        }
    }

    // Remove only files proven to be generated by the prior manifest/signature. This happens
    // before new writes so a legacy raw-label note cannot remain beside its redacted successor.
    for stale in prior_owned.difference(&current_names) {
        if let Err(error) = std::fs::remove_file(vault_dir.join(stale)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(GraphError::Io(format!(
                    "remove stale vault note {stale}: {error}"
                )));
            }
        }
    }

    write_vault_manifest(vault_dir, &current_names)?;
    for (filename, content) in rendered {
        std::fs::write(vault_dir.join(filename), content.as_bytes())
            .map_err(|error| GraphError::Io(format!("{filename}: {error}")))?;
    }
    Ok(())
}

fn generated_wiki_filename(filename: &str) -> bool {
    if filename == "index.md" {
        return true;
    }
    let Some(id) = filename
        .strip_prefix("node-")
        .and_then(|rest| rest.strip_suffix(".md"))
    else {
        return false;
    };
    !id.is_empty()
        && (id == "0" || !id.starts_with('0'))
        && id.chars().all(|character| character.is_ascii_digit())
        && id.parse::<u32>().is_ok()
}

fn generated_wiki_ownership(wiki_dir: &Path, claim_unowned: bool) -> Result<HashSet<String>> {
    let manifest_path = wiki_dir.join(WIKI_MANIFEST);
    if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|error| GraphError::Io(format!("wiki manifest read: {error}")))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| GraphError::Schema(format!("wiki manifest parse: {error}")))?;
        if value["schema"] != WIKI_MANIFEST_SCHEMA {
            return Err(GraphError::Schema(format!(
                "unsupported wiki manifest schema: {:?}",
                value["schema"]
            )));
        }
        let files = value["files"].as_array().ok_or_else(|| {
            GraphError::Schema("wiki manifest `files` must be an array".to_owned())
        })?;
        return files
            .iter()
            .map(|entry| {
                let filename = entry.as_str().ok_or_else(|| {
                    GraphError::Schema("wiki manifest filename must be a string".to_owned())
                })?;
                if !generated_wiki_filename(filename) {
                    return Err(GraphError::Guard(format!(
                        "invalid generated wiki filename in manifest: {filename:?}"
                    )));
                }
                Ok(filename.to_owned())
            })
            .collect();
    }

    if !claim_unowned {
        return Ok(HashSet::new());
    }

    let mut owned = HashSet::new();
    for entry in std::fs::read_dir(wiki_dir)
        .map_err(|error| GraphError::Io(format!("wiki inventory: {error}")))?
    {
        let entry = entry.map_err(|error| GraphError::Io(format!("wiki entry: {error}")))?;
        if !entry
            .file_type()
            .map_err(|error| GraphError::Io(format!("wiki file type: {error}")))?
            .is_file()
        {
            continue;
        }
        let filename = entry.file_name().to_string_lossy().into_owned();
        if generated_wiki_filename(&filename) {
            owned.insert(filename);
        }
    }
    Ok(owned)
}

fn write_wiki_manifest(wiki_dir: &Path, names: &HashSet<String>) -> Result<()> {
    let mut files: Vec<&str> = names.iter().map(String::as_str).collect();
    files.sort_unstable();
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "schema": WIKI_MANIFEST_SCHEMA,
        "files": files,
    }))
    .map_err(|error| GraphError::Schema(format!("wiki manifest serialize: {error}")))?;
    let temporary = wiki_dir.join(format!(".{WIKI_MANIFEST}.tmp.{}", std::process::id()));
    std::fs::write(&temporary, manifest.as_bytes())
        .map_err(|error| GraphError::Io(format!("wiki manifest temp write: {error}")))?;
    std::fs::rename(&temporary, wiki_dir.join(WIKI_MANIFEST))
        .map_err(|error| GraphError::Io(format!("wiki manifest rename: {error}")))
}

fn wiki_manifest_exists(wiki_dir: &Path) -> Result<bool> {
    let path = wiki_dir.join(WIKI_MANIFEST);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(GraphError::Io(format!("inspect wiki manifest: {error}"))),
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "wiki manifest is not a regular file: {}",
            path.display()
        )));
    }
    Ok(true)
}

pub(super) fn sync_generated_wiki(
    wiki_dir: &Path,
    rendered: &[(String, String)],
    claim_unowned: bool,
) -> Result<()> {
    std::fs::create_dir_all(wiki_dir).map_err(|error| GraphError::Io(error.to_string()))?;
    let prior_owned = generated_wiki_ownership(wiki_dir, claim_unowned)?;
    let mut current_names: HashSet<String> = HashSet::with_capacity(rendered.len());
    for (filename, _) in rendered {
        if !generated_wiki_filename(filename) || !current_names.insert(filename.clone()) {
            return Err(GraphError::Guard(format!(
                "exporter produced invalid wiki filename: {filename:?}"
            )));
        }
    }

    for filename in &current_names {
        let destination = wiki_dir.join(filename);
        match std::fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect wiki page {}: {error}",
                    destination.display()
                )))
            }
            Ok(metadata) if prior_owned.contains(filename) && metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(GraphError::Guard(format!(
                    "refusing to overwrite unowned wiki file: {}",
                    destination.display()
                )))
            }
        }
    }

    for stale in prior_owned.difference(&current_names) {
        if let Err(error) = std::fs::remove_file(wiki_dir.join(stale)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(GraphError::Io(format!(
                    "remove stale wiki page {stale}: {error}"
                )));
            }
        }
    }

    write_wiki_manifest(wiki_dir, &current_names)?;
    for (filename, content) in rendered {
        std::fs::write(wiki_dir.join(filename), content.as_bytes())
            .map_err(|error| GraphError::Io(format!("{filename}: {error}")))?;
    }
    Ok(())
}

fn existing_public_artifact(path: &Path, directory: bool) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect optional artifact {}: {error}",
                path.display()
            )))
        }
    };
    let expected_type = if directory {
        metadata.file_type().is_dir()
    } else {
        metadata.file_type().is_file()
    };
    if !expected_type {
        return Err(GraphError::Guard(format!(
            "optional artifact has unexpected file type: {}",
            path.display()
        )));
    }
    Ok(true)
}

pub(super) fn write_public_artifacts(out: &Path, graph: &Graph, opts: ExtractOpts) -> Result<()> {
    std::fs::create_dir_all(out).map_err(|error| GraphError::Io(error.to_string()))?;

    let json = habitat_graph_export::to_node_link(graph)?;
    std::fs::write(out.join("graph.json"), json.as_bytes())
        .map_err(|error| GraphError::Io(error.to_string()))?;

    let report = habitat_graph_export::render_report(graph);
    std::fs::write(out.join("GRAPH_REPORT.md"), report.as_bytes())
        .map_err(|error| GraphError::Io(error.to_string()))?;

    let html = habitat_graph_export::render_html(graph)?;
    std::fs::write(out.join("graph.html"), html.as_bytes())
        .map_err(|error| GraphError::Io(error.to_string()))?;

    let svg_path = out.join("graph.svg");
    let svg_exists = existing_public_artifact(&svg_path, false)?;
    if opts.svg || svg_exists {
        std::fs::write(
            &svg_path,
            habitat_graph_export::render_svg(graph).as_bytes(),
        )
        .map_err(|error| GraphError::Io(format!("graph.svg: {error}")))?;
    }

    let graphml_path = out.join("graph.graphml");
    let graphml_exists = existing_public_artifact(&graphml_path, false)?;
    if opts.graphml || graphml_exists {
        std::fs::write(
            &graphml_path,
            habitat_graph_export::render_graphml(graph).as_bytes(),
        )
        .map_err(|error| GraphError::Io(format!("graph.graphml: {error}")))?;
    }

    let cypher_path = out.join("graph.cypher");
    let cypher_exists = existing_public_artifact(&cypher_path, false)?;
    if opts.neo4j || cypher_exists {
        std::fs::write(
            &cypher_path,
            habitat_graph_export::render_cypher(graph).as_bytes(),
        )
        .map_err(|error| GraphError::Io(format!("graph.cypher: {error}")))?;
    }

    let wiki_dir = out.join("wiki");
    let wiki_exists = existing_public_artifact(&wiki_dir, true)?;
    let wiki_owned = wiki_exists && wiki_manifest_exists(&wiki_dir)?;
    if opts.wiki || wiki_owned {
        let rendered = habitat_graph_export::render_wiki(graph);
        sync_generated_wiki(&wiki_dir, &rendered, opts.wiki)?;
    } else if wiki_exists {
        eprintln!(
            "warning: existing wiki has no habitat-graph ownership manifest; skipping automatic refresh"
        );
    }

    Ok(())
}

/// Inner pipeline: detect → extract → build → analyze → export → write.
///
/// Returns `(node_count, edge_count, community_count)` on success.
///
/// # Errors
///
/// Returns [`GraphError::Io`] if any filesystem operation fails: directory traversal,
/// output-directory creation, or artifact write.  Propagates [`GraphError::Parse`] from the
/// tree-sitter extractor and [`GraphError::Schema`] from JSON serialization.
fn run_inner(
    dir: &Path,
    out: &Path,
    vault: Option<&Path>,
    opts: ExtractOpts,
) -> Result<(usize, usize, usize)> {
    // Detect all Rust source files under `dir`, honoring .gitignore.
    let files = habitat_graph_source::detect(dir, &["rs"])?;

    // Extract raw nodes and edges from each file (parallel, tree-sitter).
    let extractions = habitat_graph_extract::extract_files(&files)?;

    // Intern labels, resolve edges, dedup, and sort into a canonical graph.
    let mut graph = habitat_graph_build::assemble(extractions);

    // Attach Leiden communities before the final sort pass. F12: cluster on the TRUSTED subgraph
    // only — INFERRED/AMBIGUOUS edges must not inflate degree or merge communities.
    graph.communities =
        habitat_graph_analyze::detect_communities(&habitat_graph_analyze::trusted_subgraph(&graph));

    // Re-sort to canonicalize the community list (idempotent on nodes/edges).
    let graph = graph.sorted();

    write_public_artifacts(out, &graph, opts)?;

    // Optionally emit an Obsidian vault — one note per node (`[[wikilinks]]` + frontmatter/tags)
    // for Obsidian's graph view + Dataview / Juggl / Breadcrumbs.
    if let Some(vault_dir) = vault {
        let rendered = habitat_graph_export::render_vault(&graph);
        sync_generated_vault(vault_dir, &rendered)?;
    }

    Ok(graph.counts())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{run, run_artifacts, sync_generated_vault, write_vault_manifest, ExtractOpts};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Create `dir/name` (and any missing parents), writing `content`.
    fn mk_file(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&p, content).unwrap();
    }

    /// Read `out/graph.json` as a `String`.
    fn read_graph_json(out: &Path) -> String {
        fs::read_to_string(out.join("graph.json")).expect("graph.json not found")
    }

    /// Count non-overlapping occurrences of `needle` in `haystack`.
    fn count_str(haystack: &str, needle: &str) -> usize {
        let mut n = 0_usize;
        let mut pos = 0_usize;
        while let Some(idx) = haystack[pos..].find(needle) {
            n += 1;
            pos += idx + needle.len();
        }
        n
    }

    // ── T1: .rs file with two functions → exit 0 ─────────────────────────────
    // Probes: the whole pipeline runs without error for a non-trivial source file.

    #[test]
    fn single_rs_file_returns_exit_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        assert_eq!(run(src.path(), out.path(), None), 0);
    }

    // ── T1b: graph.html is written (the interactive viewer) ──────────────────
    #[test]
    fn graph_html_file_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out.path(), None);
        let html = fs::read_to_string(out.path().join("graph.html")).expect("graph.html");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("graph-data"));
    }

    // ── T1c: --vault emits an Obsidian vault (notes + _MOC) ──────────────────
    #[test]
    fn vault_emitted_when_requested() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(
            vault.path().join("_MOC.md").exists(),
            "vault MOC must exist"
        );
        // At least one node note with frontmatter + a Dataview typed edge.
        let entries: Vec<_> = fs::read_dir(vault.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
            .collect();
        assert!(
            entries.len() >= 2,
            "expected node notes + MOC, got {}",
            entries.len()
        );
        let any = fs::read_to_string(vault.path().join("a.md")).expect("a.md");
        assert!(
            any.starts_with("---\n"),
            "note must carry frontmatter: {any}"
        );
        assert!(
            any.contains("tags: [hg/node"),
            "note must carry tags: {any}"
        );
        assert!(
            any.contains(":: [["),
            "note must carry a Dataview typed edge: {any}"
        );
    }

    #[test]
    fn vault_migrates_legacy_generated_note_without_deleting_user_note() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        let raw_label = "api_key_assignment_refused";
        mk_file(src.path(), "lib.rs", &format!("fn {raw_label}() {{}}"));
        let legacy = format!(
            "---\nid: 193856898\ncrate: test\nlang: rust\nfile: \"lib.rs\"\nline: 1\ndegree: 0\ntags: [hg/node, crate/test]\n---\n\n# {raw_label}\n"
        );
        fs::write(vault.path().join(format!("{raw_label}.md")), legacy).unwrap();
        fs::write(vault.path().join("user.md"), "# User-authored note\n").unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(
            !vault.path().join(format!("{raw_label}.md")).exists(),
            "legacy raw generated note must be removed"
        );
        assert_eq!(
            fs::read_to_string(vault.path().join("user.md")).unwrap(),
            "# User-authored note\n",
            "unowned user note must remain byte-identical"
        );
        assert!(vault
            .path()
            .join(format!(
                "REDACTED_api_key_n{}.md",
                habitat_graph_core::content_id(raw_label)
            ))
            .exists());
        assert!(vault.path().join(super::VAULT_MANIFEST).exists());
    }

    #[test]
    fn vault_refuses_to_claim_user_authored_moc() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        let user_moc = "# Map of Content\n\n- [[Personal note]]\n";
        fs::write(vault.path().join("_MOC.md"), user_moc).unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 4);
        assert_eq!(
            fs::read_to_string(vault.path().join("_MOC.md")).unwrap(),
            user_moc
        );
        assert!(!vault.path().join(super::VAULT_MANIFEST).exists());
    }

    #[test]
    fn vault_manifest_removes_only_stale_generated_notes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_generated() {}");
        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(vault.path().join("old_generated.md").exists());
        fs::write(vault.path().join("user.md"), "keep me").unwrap();

        mk_file(src.path(), "lib.rs", "fn new_generated() {}");
        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(!vault.path().join("old_generated.md").exists());
        assert!(vault.path().join("new_generated.md").exists());
        assert_eq!(
            fs::read_to_string(vault.path().join("user.md")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn vault_sync_treats_missing_stale_note_as_removed() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn old_generated() {}");
        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);

        fs::remove_file(vault.path().join("old_generated.md")).unwrap();
        mk_file(src.path(), "lib.rs", "fn new_generated() {}");

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 0);
        assert!(vault.path().join("new_generated.md").exists());
        assert!(!vault.path().join("old_generated.md").exists());
    }

    #[test]
    fn vault_sync_retries_after_partial_note_write() {
        let vault = TempDir::new().unwrap();
        let legacy_secret = "api_key_assignment_refused.md";
        let prior_owned =
            std::collections::HashSet::from(["blocked.md".to_owned(), legacy_secret.to_owned()]);
        write_vault_manifest(vault.path(), &prior_owned).unwrap();
        fs::write(vault.path().join(legacy_secret), "legacy").unwrap();
        fs::create_dir(vault.path().join("blocked.md")).unwrap();
        let rendered = vec![
            ("written.md".to_owned(), "written".to_owned()),
            ("blocked.md".to_owned(), "unblocked".to_owned()),
        ];

        assert!(sync_generated_vault(vault.path(), &rendered).is_err());
        assert_eq!(
            fs::read_to_string(vault.path().join("written.md")).unwrap(),
            "written"
        );
        let journal = fs::read_to_string(vault.path().join(super::VAULT_MANIFEST)).unwrap();
        assert!(!journal.contains(legacy_secret));

        fs::remove_dir(vault.path().join("blocked.md")).unwrap();
        sync_generated_vault(vault.path(), &rendered).unwrap();
        assert_eq!(
            fs::read_to_string(vault.path().join("blocked.md")).unwrap(),
            "unblocked"
        );
    }

    #[test]
    fn vault_refuses_to_overwrite_unowned_filename_collision() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn user() {}");
        fs::write(vault.path().join("user.md"), "# Human note\n").unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 4);
        assert_eq!(
            fs::read_to_string(vault.path().join("user.md")).unwrap(),
            "# Human note\n"
        );
    }

    // ── T1d: no vault written when not requested ─────────────────────────────
    #[test]
    fn no_vault_when_not_requested() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn solo() {}");
        let _ = run(src.path(), out.path(), None);
        // out dir holds graph.json/html/report only — no _MOC.md.
        assert!(!out.path().join("_MOC.md").exists());
    }

    // ── T2: graph.json is written ────────────────────────────────────────────
    // Probes: the first artifact file is always produced on success.

    #[test]
    fn graph_json_file_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("graph.json").exists(),
            "graph.json must be created after a successful run"
        );
    }

    // ── T3: GRAPH_REPORT.md is written ───────────────────────────────────────
    // Probes: the second artifact file is always produced on success.

    #[test]
    fn graph_report_md_written() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("GRAPH_REPORT.md").exists(),
            "GRAPH_REPORT.md must be created after a successful run"
        );
    }

    // ── T4: graph.json has non-empty nodes array for .rs source ──────────────
    // Probes: at least one node is extracted from a file containing a function.
    // (Each node entry has an "id" field; links use "source"/"target", not "id".)

    #[test]
    fn graph_json_has_nodes_for_rs_source() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "must exit 0");
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"id\":"),
            "non-trivial source must produce ≥1 node (no \"id\":\" found); json={json:.200}"
        );
    }

    // ── T5: empty source dir → exit 0 ────────────────────────────────────────
    // Probes: zero files is a valid (empty-graph) success case, not an error.

    #[test]
    fn empty_source_dir_returns_exit_zero() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        assert_eq!(run(src.path(), out.path(), None), 0);
    }

    // ── T6: empty source dir → graph.json nodes is [] ────────────────────────
    // Probes: empty pipeline → empty-nodes envelope, no stale data.

    #[test]
    fn empty_source_dir_writes_empty_nodes_array() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\": []"),
            "empty source must produce \"nodes\": []; json={json:.200}"
        );
        assert!(
            !json.contains("\"id\":"),
            "empty graph must have no node id fields; json={json:.200}"
        );
    }

    // ── T7: non-existent source dir → exit 4 ─────────────────────────────────
    // Probes: detect() failure maps to exit code 4 (error path).

    #[test]
    fn nonexistent_source_dir_returns_exit_four() {
        let out = TempDir::new().unwrap();
        let phantom = Path::new("/nonexistent_habitat_graph_cli_test_xyzzy_42");
        assert_eq!(run(phantom, out.path(), None), 4);
    }

    // ── T8: nested output directory is created ────────────────────────────────
    // Probes: create_dir_all makes deeply nested out paths that don't yet exist.

    #[test]
    fn nested_out_dir_is_created() {
        let src = TempDir::new().unwrap();
        let base = TempDir::new().unwrap();
        let out = base.path().join("a").join("b").join("c");
        mk_file(src.path(), "lib.rs", "fn foo() {}");
        let rc = run(src.path(), &out, None);
        assert_eq!(rc, 0, "must exit 0 after creating nested out dir");
        assert!(
            out.join("graph.json").exists(),
            "graph.json must exist inside the nested dir"
        );
    }

    // ── T9: determinism — two runs produce byte-identical graph.json ──────────
    // Probes: sorted() + fixed Leiden seed → identical bytes on repeated calls.

    #[test]
    fn two_runs_produce_byte_identical_graph_json() {
        let src = TempDir::new().unwrap();
        let out1 = TempDir::new().unwrap();
        let out2 = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let _ = run(src.path(), out1.path(), None);
        let _ = run(src.path(), out2.path(), None);
        let j1 = fs::read(out1.path().join("graph.json")).unwrap();
        let j2 = fs::read(out2.path().join("graph.json")).unwrap();
        assert_eq!(j1, j2, "graph.json must be byte-identical across runs");
    }

    // ── T10: graph.json has the expected NetworkX envelope keys ───────────────
    // Probes: to_node_link wraps the data in the graphify-compatible envelope.

    #[test]
    fn graph_json_has_networkx_envelope_keys() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn hello() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        for key in ["\"directed\"", "\"multigraph\"", "\"nodes\"", "\"links\""] {
            assert!(
                json.contains(key),
                "graph.json must contain key {key}; json={json:.200}"
            );
        }
    }

    // ── T11: GRAPH_REPORT.md starts with the expected title ──────────────────
    // Probes: render_report output is written verbatim (correct file, not swapped).

    #[test]
    fn graph_report_starts_with_title() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn x() {}");
        let _ = run(src.path(), out.path(), None);
        let report = fs::read_to_string(out.path().join("GRAPH_REPORT.md")).unwrap();
        assert!(
            report.starts_with("# Graph Report"),
            "GRAPH_REPORT.md must start with '# Graph Report'"
        );
    }

    // ── T12: multi-item source file produces ≥2 nodes ────────────────────────
    // Probes: extractor captures all top-level items (functions + structs).

    #[test]
    fn multi_item_source_produces_multiple_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(
            src.path(),
            "lib.rs",
            "fn alpha() {} fn beta() {} struct Gamma {}",
        );
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        // Each node entry contains exactly one "id": field.
        let node_count = count_str(&json, "\"id\":");
        assert!(
            node_count >= 2,
            "3-item source must produce ≥2 nodes; got {node_count}"
        );
    }

    // ── T13: non-.rs files in the source dir are silently ignored ────────────
    // Probes: detect(&["rs"]) filters by extension; Markdown/TOML are skipped.

    #[test]
    fn non_rs_files_are_ignored() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "README.md", "# docs");
        mk_file(src.path(), "build.toml", "[x]");
        mk_file(src.path(), "lib.rs", "fn only_me() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(
            node_count, 1,
            "only the .rs file contributes nodes; got {node_count}"
        );
    }

    // ── T14: re-run overwrites existing output cleanly ────────────────────────
    // Probes: write over existing files doesn't fail or produce corrupt output.

    #[test]
    fn rerun_overwrites_existing_output_cleanly() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn alpha() {}");
        let _ = run(src.path(), out.path(), None);
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "second run must also exit 0");
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"nodes\""),
            "second run must produce valid graph.json"
        );
    }

    // ── T15: graph.json directed:true ─────────────────────────────────────────
    // Probes: the envelope correctly marks the graph as directed.

    #[test]
    fn graph_json_directed_is_true() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"directed\": true"),
            "graph.json must contain \"directed\": true"
        );
    }

    // ── T16: two-function source produces exactly 2 nodes and exit 0 ─────────
    // Probes: node count is accurate for a small, well-understood source.

    #[test]
    fn two_function_source_produces_exactly_two_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0, "must exit 0");
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(node_count, 2, "fn a + fn b must produce exactly 2 nodes");
    }

    // ── T17: multiple .rs files accumulate all nodes ──────────────────────────
    // Probes: the pipeline handles multiple input files (not just one).

    #[test]
    fn multiple_rs_files_accumulate_nodes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "a.rs", "fn fn_a() {}");
        mk_file(src.path(), "b.rs", "fn fn_b() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(
            node_count, 2,
            "two single-fn files must produce exactly 2 nodes; got {node_count}"
        );
    }

    // ── T18: graph.json links array is present in the envelope ────────────────
    // Probes: the links key is always emitted, even for empty or edge-free graphs.

    #[test]
    fn graph_json_links_array_is_present() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn lone() {}");
        let _ = run(src.path(), out.path(), None);
        let json = read_graph_json(out.path());
        assert!(
            json.contains("\"links\""),
            "graph.json must always contain a \"links\" array"
        );
    }

    // ── T19: empty source dir still writes GRAPH_REPORT.md ───────────────────
    // Probes: both artifacts are produced even for an empty graph.

    #[test]
    fn empty_source_dir_writes_report() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let _ = run(src.path(), out.path(), None);
        assert!(
            out.path().join("GRAPH_REPORT.md").exists(),
            "GRAPH_REPORT.md must exist even for an empty graph"
        );
    }

    // ── T20: deeply-nested source is detected ────────────────────────────────
    // Probes: detect() recurses into subdirectories.

    #[test]
    fn nested_source_file_is_detected() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "sub/module/deep.rs", "fn deep_fn() {}");
        let rc = run(src.path(), out.path(), None);
        assert_eq!(rc, 0);
        let json = read_graph_json(out.path());
        let node_count = count_str(&json, "\"id\":");
        assert_eq!(node_count, 1, "deeply nested .rs must be found");
    }

    // ── T21-T24: PB opt-in exporter flags emit their artifacts ───────────────

    #[test]
    fn svg_flag_emits_graph_svg() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() { b(); } fn b() {}");
        let opts = ExtractOpts {
            svg: true,
            ..ExtractOpts::default()
        };
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        let svg = fs::read_to_string(out.path().join("graph.svg")).expect("graph.svg");
        assert!(
            svg.starts_with("<svg"),
            "graph.svg must be an SVG: {svg:.60}"
        );
    }

    #[test]
    fn graphml_flag_emits_graph_graphml() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let opts = ExtractOpts {
            graphml: true,
            ..ExtractOpts::default()
        };
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        let xml = fs::read_to_string(out.path().join("graph.graphml")).expect("graph.graphml");
        assert!(xml.contains("<graphml"), "must be GraphML: {xml:.80}");
    }

    #[test]
    fn neo4j_flag_emits_graph_cypher() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let opts = ExtractOpts {
            neo4j: true,
            ..ExtractOpts::default()
        };
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        let cy = fs::read_to_string(out.path().join("graph.cypher")).expect("graph.cypher");
        assert!(
            cy.contains("cypher export"),
            "must be a Cypher script: {cy:.80}"
        );
    }

    #[test]
    fn wiki_flag_emits_wiki_dir_with_index() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        assert!(
            out.path().join("wiki").join("index.md").exists(),
            "wiki/index.md must exist"
        );
    }

    #[test]
    fn wiki_sync_removes_stale_generated_pages_and_preserves_other_files() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };
        mk_file(src.path(), "lib.rs", "fn old_generated() {}");
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);

        let wiki = out.path().join("wiki");
        let old_pages: std::collections::HashSet<String> = fs::read_dir(&wiki)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|filename| filename.starts_with("node-"))
            .collect();
        assert!(!old_pages.is_empty());
        fs::write(wiki.join("user.md"), "keep me").unwrap();

        mk_file(src.path(), "lib.rs", "fn new_generated() {}");
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        let new_pages: std::collections::HashSet<String> = fs::read_dir(&wiki)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|filename| filename.starts_with("node-"))
            .collect();

        assert!(old_pages.is_disjoint(&new_pages));
        assert!(old_pages
            .iter()
            .all(|filename| !wiki.join(filename).exists()));
        assert_eq!(fs::read_to_string(wiki.join("user.md")).unwrap(), "keep me");
    }

    #[test]
    fn default_run_preserves_unowned_wiki_pages() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        let wiki = out.path().join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(wiki.join("index.md"), "user index").unwrap();
        fs::write(wiki.join("node-1.md"), "user node").unwrap();

        assert_eq!(run(src.path(), out.path(), None), 0);
        assert_eq!(
            fs::read_to_string(wiki.join("index.md")).unwrap(),
            "user index"
        );
        assert_eq!(
            fs::read_to_string(wiki.join("node-1.md")).unwrap(),
            "user node"
        );
        assert!(!wiki.join(super::WIKI_MANIFEST).exists());
    }

    #[test]
    fn wiki_flag_claims_generated_names_and_records_ownership() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        let wiki = out.path().join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(wiki.join("index.md"), "legacy index").unwrap();
        fs::write(wiki.join("node-1.md"), "legacy node").unwrap();
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };

        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        assert_ne!(
            fs::read_to_string(wiki.join("index.md")).unwrap(),
            "legacy index"
        );
        assert!(!wiki.join("node-1.md").exists());
        let manifest = fs::read_to_string(wiki.join(super::WIKI_MANIFEST)).unwrap();
        assert!(manifest.contains(super::WIKI_MANIFEST_SCHEMA));
    }

    #[test]
    fn default_run_refreshes_existing_optional_public_artifacts() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let raw_label = "api_key_assignment_refused";
        mk_file(src.path(), "lib.rs", &format!("fn {raw_label}() {{}}"));
        let opts = ExtractOpts {
            svg: true,
            graphml: true,
            neo4j: true,
            wiki: true,
        };
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);

        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            fs::write(out.path().join(artifact), raw_label).unwrap();
        }
        let wiki = out.path().join("wiki");
        fs::write(wiki.join("index.md"), raw_label).unwrap();
        let node_id = habitat_graph_core::content_id(raw_label);
        let stale_id = if node_id == u32::MAX {
            node_id - 1
        } else {
            node_id + 1
        };
        fs::write(wiki.join(format!("node-{stale_id}.md")), raw_label).unwrap();
        let mut owned = super::generated_wiki_ownership(&wiki, false).unwrap();
        owned.insert(format!("node-{stale_id}.md"));
        super::write_wiki_manifest(&wiki, &owned).unwrap();
        fs::write(wiki.join("user.md"), "keep me").unwrap();

        assert_eq!(run(src.path(), out.path(), None), 0);
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            assert!(!fs::read_to_string(out.path().join(artifact))
                .unwrap()
                .contains(raw_label));
        }
        assert!(!wiki.join(format!("node-{stale_id}.md")).exists());
        assert_eq!(fs::read_to_string(wiki.join("user.md")).unwrap(), "keep me");
        for entry in fs::read_dir(&wiki).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == "user.md" {
                continue;
            }
            assert!(!fs::read_to_string(entry.path())
                .unwrap()
                .contains(raw_label));
        }
    }

    #[test]
    fn default_run_emits_no_optional_artifacts() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn a() {}");
        let _ = run(src.path(), out.path(), None);
        assert!(!out.path().join("graph.svg").exists(), "no svg by default");
        assert!(
            !out.path().join("graph.graphml").exists(),
            "no graphml by default"
        );
        assert!(
            !out.path().join("graph.cypher").exists(),
            "no cypher by default"
        );
        assert!(!out.path().join("wiki").exists(), "no wiki by default");
    }
}
