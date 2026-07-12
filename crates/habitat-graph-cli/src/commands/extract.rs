//! The `extract` command — the full pipeline (detect → extract → build → analyze → export → write).
//!
//! Core and optional artifacts are public redacted projections. Hidden ownership manifests let a
//! later `extract`, `update`, or `watch` refresh previously generated optional/wiki output without
//! deleting or overwriting unowned files. Obsidian vault sync uses the same fail-closed ownership
//! rule, including conservative recognition of the exact legacy generated-note layout.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Component, Path};

use habitat_graph_core::{Graph, GraphError, Result};

/// Ownership manifest for generated Obsidian notes. Only files recorded here (or recognized by
/// the conservative legacy signature during first migration) may be removed on a later sync.
const VAULT_MANIFEST: &str = ".habitat-graph-generated.json";
const LEGACY_VAULT_MANIFEST_SCHEMA: &str = "habitat-graph.vault-manifest.v1";
const PENDING_VAULT_MANIFEST_SCHEMA: &str = "habitat-graph.vault-manifest.v2";
const VAULT_MANIFEST_SCHEMA: &str = "habitat-graph.vault-manifest.v3";
const WIKI_MANIFEST: &str = ".habitat-graph-generated.json";
const LEGACY_WIKI_MANIFEST_SCHEMA: &str = "habitat-graph.wiki-manifest.v1";
const PENDING_WIKI_MANIFEST_SCHEMA: &str = "habitat-graph.wiki-manifest.v2";
const WIKI_MANIFEST_SCHEMA: &str = "habitat-graph.wiki-manifest.v3";
const OPTIONAL_ARTIFACT_MANIFEST: &str = ".habitat-graph-artifacts.json";
const OPTIONAL_ARTIFACT_MANIFEST_SCHEMA: &str = "habitat-graph.artifact-manifest.v1";
const OPTIONAL_ARTIFACTS: &[&str] = &["graph.svg", "graph.graphml", "graph.cypher"];

/// Optional PB exporter artifacts to emit alongside the always-written core artifacts.
///
/// All default to `false` (F13: human-facing exporters never burden the agent-critical path —
/// `graph.json` + `GRAPH_REPORT.md` + `graph.html` are always written; these are opt-in). Once an
/// optional artifact is explicitly adopted, its ownership manifest keeps it refreshed on later
/// default runs even when the flag is omitted.
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

struct GeneratedVaultNode {
    id: u32,
    community: Option<u32>,
    label: String,
}

fn valid_legacy_yaml_string(value: &str) -> bool {
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '"' {
            return false;
        }
        if character == '\\' && !matches!(characters.next(), Some('\\' | '"')) {
            return false;
        }
    }
    true
}

fn canonical_legacy_number<T>(value: &str) -> Option<T>
where
    T: std::str::FromStr + ToString,
{
    let parsed = value.parse::<T>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

/// Recognizes the exact layout emitted by habitat-graph node notes before ownership manifests.
fn parse_generated_vault_node_note(content: &str) -> Option<GeneratedVaultNode> {
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return None;
    }
    let id = canonical_legacy_number::<u32>(lines.next()?.strip_prefix("id: ")?)?;

    let mut field = lines.next()?;
    let community = if let Some(value) = field.strip_prefix("community: ") {
        let community = canonical_legacy_number::<u32>(value)?;
        field = lines.next()?;
        Some(community)
    } else {
        None
    };

    let krate = field.strip_prefix("crate: ")?;
    if !krate
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
    {
        return None;
    }
    let lang = lines.next()?.strip_prefix("lang: ")?;
    if !matches!(lang, "rust" | "python" | "js" | "other") {
        return None;
    }
    let file = lines.next()?.strip_prefix("file: \"")?.strip_suffix('"')?;
    if !valid_legacy_yaml_string(file) {
        return None;
    }
    let line = lines.next()?.strip_prefix("line: ")?;
    canonical_legacy_number::<u32>(line)?;
    let degree = lines.next()?.strip_prefix("degree: ")?;
    canonical_legacy_number::<u64>(degree)?;

    let mut tags = format!("tags: [hg/node, crate/{krate}, lang/{lang}");
    if let Some(community) = community {
        let _ = write!(tags, ", community/{community}");
    }
    tags.push(']');
    if lines.next()? != tags || lines.next()? != "---" || !lines.next()?.is_empty() {
        return None;
    }

    let label = lines.next()?.strip_prefix("# ")?.to_owned();
    if !lines.next()?.is_empty() {
        return None;
    }
    let location = lines.next()?;
    let location_suffix = format!(":{line}` · crate `{krate}` · degree {degree}");
    if !location.starts_with("> `") || !location.ends_with(&location_suffix) {
        return None;
    }
    if !lines.next()?.is_empty() || lines.next()? != "## Links" {
        return None;
    }
    for link in lines {
        let (_, target) = link
            .strip_prefix("- ")
            .and_then(|link| link.split_once(":: [["))?;
        if !target.ends_with("]]") {
            return None;
        }
    }

    Some(GeneratedVaultNode {
        id,
        community,
        label,
    })
}

fn parse_generated_vault_moc(content: &str) -> Option<Vec<(Option<u32>, Vec<&str>)>> {
    if !content.ends_with('\n') {
        return None;
    }

    let mut source_lines = content.lines();
    if source_lines.next() != Some("# Map of Content") {
        return None;
    }
    let Some(first_separator) = source_lines.next() else {
        return Some(Vec::new());
    };
    if !first_separator.is_empty() {
        return None;
    }

    let mut heading = source_lines.next()?;
    let mut sections = Vec::new();
    let mut last_community = None;
    let mut saw_unclustered = false;
    loop {
        let community = if heading == "## Unclustered" {
            if saw_unclustered {
                return None;
            }
            saw_unclustered = true;
            None
        } else {
            if saw_unclustered {
                return None;
            }
            let value = heading.strip_prefix("## community c")?;
            let community = canonical_legacy_number::<u32>(value)?;
            if last_community.is_some_and(|previous| community <= previous) {
                return None;
            }
            last_community = Some(community);
            Some(community)
        };
        if source_lines.next() != Some("") {
            return None;
        }

        let mut section_links = Vec::new();
        let next_heading = loop {
            match source_lines.next() {
                Some(line) if line.starts_with("- [[") && line.ends_with("]]") => {
                    section_links.push(line);
                }
                Some("") => break Some(source_lines.next()?),
                Some(_) => return None,
                None => break None,
            }
        };
        if section_links.is_empty() {
            return None;
        }
        sections.push((community, section_links));

        let Some(next_heading) = next_heading else {
            break;
        };
        heading = next_heading;
    }
    Some(sections)
}

fn generated_vault_moc_link(
    filename: &str,
    note: &GeneratedVaultNode,
    filename_targets: bool,
) -> String {
    if !filename_targets {
        return format!("- [[{}]]", note.label);
    }
    let target = filename.strip_suffix(".md").unwrap_or(filename);
    if target == note.label {
        format!("- [[{target}]]")
    } else {
        format!("- [[{target}|{}]]", note.label)
    }
}

fn generated_vault_moc_section_matches(
    links: &[&str],
    community: Option<u32>,
    notes: &[(String, GeneratedVaultNode)],
    filename_targets: bool,
) -> bool {
    if links.is_empty() {
        return false;
    }
    let mut candidates: Vec<_> = notes
        .iter()
        .filter(|(_, note)| note.community == community)
        .collect();
    candidates.sort_unstable_by_key(|(_, note)| note.id);
    candidates.len() == links.len()
        && candidates
            .iter()
            .zip(links)
            .all(|((filename, note), link)| {
                generated_vault_moc_link(filename, note, filename_targets) == *link
            })
}

fn is_generated_vault_moc(content: &str, notes: &[(String, GeneratedVaultNode)]) -> bool {
    let Some(sections) = parse_generated_vault_moc(content) else {
        return false;
    };
    let mut seen_ids = HashSet::with_capacity(notes.len());
    if notes.iter().any(|(_, note)| !seen_ids.insert(note.id)) {
        return false;
    }
    if sections.is_empty() {
        return false;
    }

    [false, true].into_iter().any(|filename_targets| {
        sections.iter().map(|(_, links)| links.len()).sum::<usize>() == notes.len()
            && sections.iter().all(|(community, links)| {
                generated_vault_moc_section_matches(links, *community, notes, filename_targets)
            })
    })
}

fn read_generated_manifest(
    directory: &Path,
    manifest_name: &str,
    legacy_schema: &str,
    pending_schema: &str,
    schema: &str,
    valid_filename: fn(&str) -> bool,
    context: &str,
) -> Result<Option<HashSet<String>>> {
    let manifest_path = directory.join(manifest_name);
    let metadata = match std::fs::symlink_metadata(&manifest_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect {context} manifest: {error}"
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "{context} manifest is not a regular file: {}",
            manifest_path.display()
        )));
    }
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| GraphError::Io(format!("{context} manifest read: {error}")))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| GraphError::Schema(format!("{context} manifest parse: {error}")))?;
    let manifest_schema = value["schema"].as_str();
    if !matches!(manifest_schema, Some(candidate) if candidate == legacy_schema || candidate == pending_schema || candidate == schema)
    {
        return Err(GraphError::Schema(format!(
            "unsupported {context} manifest schema: {:?}",
            value["schema"]
        )));
    }
    let files = value["files"].as_array().ok_or_else(|| {
        GraphError::Schema(format!("{context} manifest `files` must be an array"))
    })?;
    let mut owned = HashSet::with_capacity(files.len());
    for entry in files {
        let filename = entry.as_str().ok_or_else(|| {
            GraphError::Schema(format!("{context} manifest filename must be a string"))
        })?;
        if !valid_filename(filename) || !owned.insert(filename.to_owned()) {
            return Err(GraphError::Guard(format!(
                "invalid generated {context} filename in manifest: {filename:?}"
            )));
        }
    }

    if matches!(manifest_schema, Some(candidate) if candidate == pending_schema || candidate == schema)
    {
        let pending = value["pending"].as_object().ok_or_else(|| {
            GraphError::Schema(format!("{context} manifest `pending` must be an object"))
        })?;
        recover_hashed_owned_files(
            directory,
            pending,
            &mut owned,
            valid_filename,
            context,
            "pending",
        )?;
        if manifest_schema == Some(pending_schema) && !pending.is_empty() {
            return Err(GraphError::Guard(format!(
                "cannot safely resume legacy pending {context} ownership transaction"
            )));
        }
    }
    if manifest_schema == Some(schema) {
        let deleting = value["deleting"].as_object().ok_or_else(|| {
            GraphError::Schema(format!("{context} manifest `deleting` must be an object"))
        })?;
        recover_hashed_owned_files(
            directory,
            deleting,
            &mut owned,
            valid_filename,
            context,
            "deleting",
        )?;
    }
    Ok(Some(owned))
}

fn recover_hashed_owned_files(
    directory: &Path,
    entries: &serde_json::Map<String, serde_json::Value>,
    owned: &mut HashSet<String>,
    valid_filename: fn(&str) -> bool,
    context: &str,
    phase: &str,
) -> Result<()> {
    for (filename, expected) in entries {
        let expected = expected.as_str().ok_or_else(|| {
            GraphError::Schema(format!("{context} manifest {phase} hash must be a string"))
        })?;
        if !valid_filename(filename) || !is_content_generation(expected) || owned.contains(filename)
        {
            return Err(GraphError::Guard(format!(
                "invalid {phase} generated {context} file: {filename:?}"
            )));
        }
        let destination = directory.join(filename);
        let metadata = match std::fs::symlink_metadata(&destination) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect {phase} {context} file {filename}: {error}"
                )))
            }
        };
        if metadata.file_type().is_file() {
            let bytes = std::fs::read(&destination).map_err(|error| {
                GraphError::Io(format!("read {phase} {context} file {filename}: {error}"))
            })?;
            if content_generation(&bytes) == expected {
                owned.insert(filename.clone());
            }
        }
    }
    Ok(())
}

fn write_generated_manifest(
    directory: &Path,
    manifest_name: &str,
    schema: &str,
    names: &HashSet<String>,
    pending: &BTreeMap<String, String>,
    deleting: &BTreeMap<String, String>,
    context: &str,
) -> Result<()> {
    let mut files: Vec<&str> = names.iter().map(String::as_str).collect();
    files.sort_unstable();
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "schema": schema,
        "files": files,
        "pending": pending,
        "deleting": deleting,
    }))
    .map_err(|error| GraphError::Schema(format!("{context} manifest serialize: {error}")))?;
    super::atomic_file::write(
        &directory.join(manifest_name),
        manifest.as_bytes(),
        false,
        &format!("{context} manifest"),
    )
}

fn pending_generated_files(
    rendered: &[(String, String)],
    prior_owned: &HashSet<String>,
) -> BTreeMap<String, String> {
    rendered
        .iter()
        .filter(|(filename, _)| !prior_owned.contains(filename))
        .map(|(filename, content)| (filename.clone(), content_generation(content.as_bytes())))
        .collect()
}

fn pending_deleted_files(
    directory: &Path,
    prior_owned: &HashSet<String>,
    current_names: &HashSet<String>,
    context: &str,
) -> Result<BTreeMap<String, String>> {
    let mut deleting = BTreeMap::new();
    for filename in prior_owned.difference(current_names) {
        let destination = directory.join(filename);
        let metadata = match std::fs::symlink_metadata(&destination) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect stale {context} file {filename}: {error}"
                )))
            }
        };
        if !metadata.file_type().is_file() {
            return Err(GraphError::Guard(format!(
                "refusing to remove unsafe stale {context} file: {}",
                destination.display()
            )));
        }
        let bytes = std::fs::read(&destination)
            .map_err(|error| GraphError::Io(format!("read stale {context} file: {error}")))?;
        deleting.insert(filename.clone(), content_generation(&bytes));
    }
    Ok(deleting)
}

fn content_generation(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn is_content_generation(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Loads the generated-file ownership set, migrating conservative legacy note signatures when no
/// manifest exists. An invalid existing manifest fails closed rather than guessing ownership.
fn generated_vault_ownership(vault_dir: &Path) -> Result<HashSet<String>> {
    if let Some(owned) = read_generated_manifest(
        vault_dir,
        VAULT_MANIFEST,
        LEGACY_VAULT_MANIFEST_SCHEMA,
        PENDING_VAULT_MANIFEST_SCHEMA,
        VAULT_MANIFEST_SCHEMA,
        safe_vault_filename,
        "vault",
    )? {
        return Ok(owned);
    }

    let mut owned = HashSet::new();
    let mut generated_notes = Vec::new();
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
        if let Some(note) = parse_generated_vault_node_note(&content) {
            owned.insert(filename.clone());
            generated_notes.push((filename, note));
        }
    }
    let moc_path = vault_dir.join("_MOC.md");
    if std::fs::symlink_metadata(&moc_path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        let content = std::fs::read_to_string(&moc_path)
            .map_err(|error| GraphError::Io(format!("legacy vault MOC: {error}")))?;
        if is_generated_vault_moc(&content, &generated_notes) {
            owned.insert("_MOC.md".to_owned());
        }
    }
    Ok(owned)
}

fn write_vault_manifest(vault_dir: &Path, names: &HashSet<String>) -> Result<()> {
    write_vault_manifest_state(vault_dir, names, &BTreeMap::new(), &BTreeMap::new())
}

fn write_vault_manifest_state(
    vault_dir: &Path,
    names: &HashSet<String>,
    pending: &BTreeMap<String, String>,
    deleting: &BTreeMap<String, String>,
) -> Result<()> {
    write_generated_manifest(
        vault_dir,
        VAULT_MANIFEST,
        VAULT_MANIFEST_SCHEMA,
        names,
        pending,
        deleting,
        "vault",
    )
}

/// Synchronizes generated notes exactly while preserving every unowned/user-authored file.
fn sync_generated_vault(vault_dir: &Path, rendered: &[(String, String)]) -> Result<()> {
    std::fs::create_dir_all(vault_dir).map_err(|error| GraphError::Io(error.to_string()))?;
    let prior_owned = generated_vault_ownership(vault_dir)?;
    let mut current_names = HashSet::with_capacity(rendered.len());
    for (filename, _) in rendered {
        if !safe_vault_filename(filename) || !current_names.insert(filename.clone()) {
            return Err(GraphError::Guard(format!(
                "exporter produced unsafe vault filename: {filename:?}"
            )));
        }
        let destination = vault_dir.join(filename);
        match std::fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(GraphError::Io(format!(
                    "inspect vault note {}: {error}",
                    destination.display()
                )))
            }
            Ok(metadata) if prior_owned.contains(filename) && metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(GraphError::Guard(format!(
                    "refusing to overwrite unsafe or unowned vault file: {}",
                    destination.display()
                )))
            }
        }
    }

    let pending = pending_generated_files(rendered, &prior_owned);
    let deleting = pending_deleted_files(vault_dir, &prior_owned, &current_names, "vault")?;
    let retained = prior_owned.intersection(&current_names).cloned().collect();
    write_vault_manifest_state(vault_dir, &retained, &pending, &deleting)?;

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

    for (filename, content) in rendered {
        super::atomic_file::write(
            &vault_dir.join(filename),
            content.as_bytes(),
            false,
            filename,
        )?;
    }
    write_vault_manifest(vault_dir, &current_names)
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

#[derive(Debug)]
struct LegacyWikiLink {
    label: String,
    target: String,
    relation: Option<String>,
}

#[derive(Debug)]
struct LegacyWikiNode {
    title: String,
    outbound: Vec<LegacyWikiLink>,
    inbound: Vec<LegacyWikiLink>,
}

fn legacy_wiki_lines(content: &str) -> Option<Vec<&str>> {
    let body = content.strip_suffix('\n')?;
    if body.contains('\r') {
        return None;
    }
    Some(body.split('\n').collect())
}

fn parse_legacy_wiki_link(line: &str, with_relation: bool) -> Option<LegacyWikiLink> {
    let rest = line.strip_prefix("- [")?;
    let (label, rest) = rest.split_once("](node-")?;
    let (id, suffix) = rest.split_once(".md)")?;
    let id = canonical_legacy_number::<u32>(id)?;
    let relation = if with_relation {
        Some(suffix.strip_prefix(" (")?.strip_suffix(')')?.to_owned())
    } else {
        if !suffix.is_empty() {
            return None;
        }
        None
    };
    Some(LegacyWikiLink {
        label: label.to_owned(),
        target: format!("node-{id}.md"),
        relation,
    })
}

fn parse_legacy_wiki_edge_section(lines: &[&str]) -> Option<Vec<LegacyWikiLink>> {
    if lines == ["_none_"] {
        return Some(Vec::new());
    }
    if lines.is_empty() {
        return None;
    }
    lines
        .iter()
        .map(|line| parse_legacy_wiki_link(line, true))
        .collect()
}

fn parse_legacy_wiki_node(content: &str) -> Option<LegacyWikiNode> {
    let lines = legacy_wiki_lines(content)?;
    if lines.len() < 11
        || !lines[1].is_empty()
        || !lines[3].is_empty()
        || lines[4] != "## Outbound"
        || !lines[5].is_empty()
    {
        return None;
    }
    let title = lines[0].strip_prefix("# ")?.to_owned();
    let source = lines[2].strip_prefix("Source: `")?;
    let (_, line) = source.rsplit_once("` line ")?;
    canonical_legacy_number::<u32>(line)?;

    let inbound = lines[6..]
        .windows(3)
        .position(|window| window == ["", "## Inbound", ""])?
        + 6;
    let outbound = parse_legacy_wiki_edge_section(&lines[6..inbound])?;
    let inbound = parse_legacy_wiki_edge_section(&lines[inbound + 3..])?;
    Some(LegacyWikiNode {
        title,
        outbound,
        inbound,
    })
}

fn parse_legacy_wiki_index(content: &str) -> Option<Vec<LegacyWikiLink>> {
    let lines = legacy_wiki_lines(content)?;
    if lines.len() < 3 || lines[0] != "# Index" || !lines[1].is_empty() {
        return None;
    }
    lines[2..]
        .iter()
        .map(|line| parse_legacy_wiki_link(line, false))
        .collect()
}

fn legacy_wiki_link_text(title: &str) -> String {
    let mut escaped = String::with_capacity(title.len());
    for character in title.chars() {
        if matches!(character, '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn legacy_wiki_link_matches(
    link: &LegacyWikiLink,
    nodes: &BTreeMap<String, LegacyWikiNode>,
    indexed: &HashSet<String>,
) -> bool {
    indexed.contains(&link.target)
        && nodes.get(&link.target).is_some_and(|target| {
            link.label == legacy_wiki_link_text(&target.title) && link.relation.is_some()
        })
}

fn legacy_wiki_reciprocal_matches(
    link: &LegacyWikiLink,
    source: &str,
    source_title: &str,
    nodes: &BTreeMap<String, LegacyWikiNode>,
    outbound: bool,
) -> bool {
    let Some(target) = nodes.get(&link.target) else {
        return false;
    };
    let reciprocal = if outbound {
        &target.inbound
    } else {
        &target.outbound
    };
    let source_label = legacy_wiki_link_text(source_title);
    reciprocal.iter().any(|candidate| {
        candidate.target == source
            && candidate.label == source_label
            && candidate.relation == link.relation
    })
}

fn legacy_generated_wiki_ownership(contents: &BTreeMap<String, String>) -> HashSet<String> {
    let Some(index_links) = contents
        .get("index.md")
        .and_then(|content| parse_legacy_wiki_index(content))
    else {
        return HashSet::new();
    };
    let nodes: BTreeMap<String, LegacyWikiNode> = contents
        .iter()
        .filter(|(filename, _)| filename.as_str() != "index.md")
        .filter_map(|(filename, content)| {
            parse_legacy_wiki_node(content).map(|node| (filename.clone(), node))
        })
        .collect();
    let mut indexed = HashSet::with_capacity(index_links.len());
    for link in &index_links {
        let Some(node) = nodes.get(&link.target) else {
            return HashSet::new();
        };
        if !indexed.insert(link.target.clone())
            || link.label != legacy_wiki_link_text(&node.title)
            || link.relation.is_some()
        {
            return HashSet::new();
        }
    }
    if indexed.is_empty() {
        return HashSet::new();
    }

    for filename in &indexed {
        let node = &nodes[filename];
        if node
            .outbound
            .iter()
            .any(|link| !legacy_wiki_link_matches(link, &nodes, &indexed))
            || node
                .inbound
                .iter()
                .any(|link| !legacy_wiki_link_matches(link, &nodes, &indexed))
            || node.outbound.iter().any(|link| {
                !legacy_wiki_reciprocal_matches(link, filename, &node.title, &nodes, true)
            })
            || node.inbound.iter().any(|link| {
                !legacy_wiki_reciprocal_matches(link, filename, &node.title, &nodes, false)
            })
        {
            return HashSet::new();
        }
    }

    let mut owned = indexed;
    owned.insert("index.md".to_owned());
    owned
}

fn generated_wiki_ownership(wiki_dir: &Path, claim_unowned: bool) -> Result<HashSet<String>> {
    if let Some(owned) = read_generated_manifest(
        wiki_dir,
        WIKI_MANIFEST,
        LEGACY_WIKI_MANIFEST_SCHEMA,
        PENDING_WIKI_MANIFEST_SCHEMA,
        WIKI_MANIFEST_SCHEMA,
        generated_wiki_filename,
        "wiki",
    )? {
        return Ok(owned);
    }

    if !claim_unowned {
        return Ok(HashSet::new());
    }

    let mut owned = HashSet::new();
    let mut unsigned = BTreeMap::new();
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
            let content = std::fs::read_to_string(entry.path()).map_err(|error| {
                GraphError::Io(format!("unowned wiki page {filename}: {error}"))
            })?;
            if has_generated_wiki_signature(&content) {
                owned.insert(filename);
            } else {
                unsigned.insert(filename, content);
            }
        }
    }
    owned.extend(legacy_generated_wiki_ownership(&unsigned));
    Ok(owned)
}

fn write_wiki_manifest(wiki_dir: &Path, names: &HashSet<String>) -> Result<()> {
    write_wiki_manifest_state(wiki_dir, names, &BTreeMap::new(), &BTreeMap::new())
}

fn write_wiki_manifest_state(
    wiki_dir: &Path,
    names: &HashSet<String>,
    pending: &BTreeMap<String, String>,
    deleting: &BTreeMap<String, String>,
) -> Result<()> {
    write_generated_manifest(
        wiki_dir,
        WIKI_MANIFEST,
        WIKI_MANIFEST_SCHEMA,
        names,
        pending,
        deleting,
        "wiki",
    )
}

fn has_generated_wiki_signature(content: &str) -> bool {
    let mut lines = content.lines();
    lines.next().is_some()
        && lines.next() == Some("")
        && lines.next() == Some(habitat_graph_export::wiki::GENERATED_WIKI_SIGNATURE)
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

/// Synchronizes generated wiki pages while preserving every unowned path.
///
/// `claim_unowned` permits a first explicit `--wiki` run to adopt only signed pages or a complete,
/// reciprocally linked legacy wiki. Later runs trust the ownership manifest; malformed manifests,
/// unsafe filenames, and collisions with unowned files fail closed.
///
/// # Errors
///
/// Returns [`GraphError::Io`] for filesystem failures, [`GraphError::Schema`] for invalid
/// manifests, or [`GraphError::Guard`] when a path cannot safely be claimed or replaced.
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

    let pending = pending_generated_files(rendered, &prior_owned);
    let deleting = pending_deleted_files(wiki_dir, &prior_owned, &current_names, "wiki")?;
    let retained = prior_owned.intersection(&current_names).cloned().collect();
    write_wiki_manifest_state(wiki_dir, &retained, &pending, &deleting)?;

    for stale in prior_owned.difference(&current_names) {
        if let Err(error) = std::fs::remove_file(wiki_dir.join(stale)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(GraphError::Io(format!(
                    "remove stale wiki page {stale}: {error}"
                )));
            }
        }
    }

    for (filename, content) in rendered {
        super::atomic_file::write(
            &wiki_dir.join(filename),
            content.as_bytes(),
            false,
            filename,
        )?;
    }
    write_wiki_manifest(wiki_dir, &current_names)
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

fn load_optional_artifact_ownership(out: &Path) -> Result<Option<HashSet<String>>> {
    let path = out.join(OPTIONAL_ARTIFACT_MANIFEST);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GraphError::Io(format!(
                "inspect optional artifact manifest: {error}"
            )))
        }
    };
    if !metadata.file_type().is_file() {
        return Err(GraphError::Guard(format!(
            "optional artifact manifest is not a regular file: {}",
            path.display()
        )));
    }

    let text = std::fs::read_to_string(&path)
        .map_err(|error| GraphError::Io(format!("optional artifact manifest read: {error}")))?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
        GraphError::Schema(format!("optional artifact manifest parse: {error}"))
    })?;
    if value["schema"] != OPTIONAL_ARTIFACT_MANIFEST_SCHEMA {
        return Err(GraphError::Schema(format!(
            "unsupported optional artifact manifest schema: {:?}",
            value["schema"]
        )));
    }
    let files = value["files"].as_array().ok_or_else(|| {
        GraphError::Schema("optional artifact manifest `files` must be an array".to_owned())
    })?;
    files
        .iter()
        .map(|entry| {
            let filename = entry.as_str().ok_or_else(|| {
                GraphError::Schema(
                    "optional artifact manifest filename must be a string".to_owned(),
                )
            })?;
            if !OPTIONAL_ARTIFACTS.contains(&filename) {
                return Err(GraphError::Guard(format!(
                    "invalid optional artifact filename in manifest: {filename:?}"
                )));
            }
            Ok(filename.to_owned())
        })
        .collect::<Result<HashSet<_>>>()
        .map(Some)
}

fn write_optional_artifact_manifest(out: &Path, names: &HashSet<String>) -> Result<()> {
    let mut files: Vec<&str> = names.iter().map(String::as_str).collect();
    files.sort_unstable();
    let manifest = serde_json::to_string_pretty(&serde_json::json!({
        "schema": OPTIONAL_ARTIFACT_MANIFEST_SCHEMA,
        "files": files,
    }))
    .map_err(|error| {
        GraphError::Schema(format!("optional artifact manifest serialize: {error}"))
    })?;
    super::atomic_file::write(
        &out.join(OPTIONAL_ARTIFACT_MANIFEST),
        manifest.as_bytes(),
        false,
        "optional artifact manifest",
    )
}

fn select_optional_artifact(
    path: &Path,
    filename: &str,
    flag: &str,
    requested: bool,
    owned: &mut HashSet<String>,
) -> Result<bool> {
    if requested {
        existing_public_artifact(path, false)?;
        owned.insert(filename.to_owned());
        return Ok(true);
    }
    if !owned.contains(filename) {
        if existing_public_artifact(path, false)? {
            eprintln!(
                "warning: existing unowned optional artifact {} was not refreshed; run extract with {flag} to adopt and replace it",
                path.display()
            );
        }
        return Ok(false);
    }
    if existing_public_artifact(path, false)? {
        Ok(true)
    } else {
        owned.remove(filename);
        Ok(false)
    }
}

/// Atomically writes the core redacted artifacts and refreshes owned optional/wiki projections.
///
/// An explicit option adopts the corresponding optional artifact. On subsequent calls its hidden
/// ownership manifest refreshes it even with default options. Existing unowned artifacts are left
/// untouched (with a warning), and invalid manifests or unsafe file types fail closed.
///
/// # Errors
///
/// Returns [`GraphError::Io`] for filesystem failures, [`GraphError::Schema`] for serialization or
/// manifest failures, and [`GraphError::Guard`] for unsafe or unowned replacement targets.
pub(super) fn write_public_artifacts(out: &Path, graph: &Graph, opts: ExtractOpts) -> Result<()> {
    std::fs::create_dir_all(out).map_err(|error| GraphError::Io(error.to_string()))?;

    let prior_optional_ownership = load_optional_artifact_ownership(out)?;
    let had_optional_manifest = prior_optional_ownership.is_some();
    let mut optional_owned = prior_optional_ownership.unwrap_or_default();
    let svg_path = out.join("graph.svg");
    let svg_selected = select_optional_artifact(
        &svg_path,
        "graph.svg",
        "--svg",
        opts.svg,
        &mut optional_owned,
    )?;
    let graphml_path = out.join("graph.graphml");
    let graphml_selected = select_optional_artifact(
        &graphml_path,
        "graph.graphml",
        "--graphml",
        opts.graphml,
        &mut optional_owned,
    )?;
    let cypher_path = out.join("graph.cypher");
    let cypher_selected = select_optional_artifact(
        &cypher_path,
        "graph.cypher",
        "--neo4j",
        opts.neo4j,
        &mut optional_owned,
    )?;

    let json = habitat_graph_export::to_node_link(graph)?;
    super::atomic_file::write(
        &out.join("graph.json"),
        json.as_bytes(),
        false,
        "graph.json",
    )?;

    let report = habitat_graph_export::render_report(graph);
    super::atomic_file::write(
        &out.join("GRAPH_REPORT.md"),
        report.as_bytes(),
        false,
        "GRAPH_REPORT.md",
    )?;

    let html = habitat_graph_export::render_html(graph)?;
    super::atomic_file::write(
        &out.join("graph.html"),
        html.as_bytes(),
        false,
        "graph.html",
    )?;

    if svg_selected {
        super::atomic_file::write(
            &svg_path,
            habitat_graph_export::render_svg(graph).as_bytes(),
            false,
            "graph.svg",
        )?;
    }

    if graphml_selected {
        super::atomic_file::write(
            &graphml_path,
            habitat_graph_export::render_graphml(graph).as_bytes(),
            false,
            "graph.graphml",
        )?;
    }

    if cypher_selected {
        super::atomic_file::write(
            &cypher_path,
            habitat_graph_export::render_cypher(graph).as_bytes(),
            false,
            "graph.cypher",
        )?;
    }

    if had_optional_manifest || opts.svg || opts.graphml || opts.neo4j {
        write_optional_artifact_manifest(out, &optional_owned)?;
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

    std::fs::create_dir_all(out).map_err(|error| GraphError::Io(error.to_string()))?;
    let legacy_state = out.join(".habitat-graph-state.json");
    #[cfg(unix)]
    let state_path = super::private_state::path_for_output(&out.join("graph.json"), &legacy_state)?;
    #[cfg(not(unix))]
    let state_path = legacy_state;
    let _output_lock = super::private_state::acquire_output_lock(&state_path)?;
    super::private_state::ensure_no_pending_add_journals(&state_path)?;
    super::private_state::ensure_no_pending_update_journals(&state_path)?;
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

    use super::{
        run, run_artifacts, sync_generated_vault, sync_generated_wiki, write_vault_manifest,
        write_wiki_manifest, ExtractOpts,
    };

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

    fn legacy_vault_note(id: u32, community: Option<u32>, label: &str) -> String {
        let community_field =
            community.map_or_else(String::new, |value| format!("community: {value}\n"));
        let community_tag =
            community.map_or_else(String::new, |value| format!(", community/{value}"));
        format!(
            "---\nid: {id}\n{community_field}crate: test\nlang: rust\nfile: \"lib.rs\"\nline: 1\ndegree: 0\ntags: [hg/node, crate/test, lang/rust{community_tag}]\n---\n\n# {label}\n\n> `lib.rs:1` · crate `test` · degree 0\n\n## Links\n"
        )
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
            "---\nid: 193856898\ncrate: test\nlang: rust\nfile: \"lib.rs\"\nline: 1\ndegree: 0\ntags: [hg/node, crate/test, lang/rust]\n---\n\n# {raw_label}\n\n> `lib.rs:1` · crate `test` · degree 0\n\n## Links\n"
        );
        fs::write(vault.path().join(format!("{raw_label}.md")), legacy).unwrap();
        fs::write(
            vault.path().join("_MOC.md"),
            format!("# Map of Content\n\n## Unclustered\n\n- [[{raw_label}]]\n"),
        )
        .unwrap();
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
        assert!(!fs::read_to_string(vault.path().join("_MOC.md"))
            .unwrap()
            .contains(raw_label));
        assert!(vault.path().join(super::VAULT_MANIFEST).exists());
    }

    #[test]
    fn legacy_vault_ownership_requires_the_exact_generated_layout() {
        let valid = "---\nid: 1\ncommunity: 2\ncrate: test\nlang: rust\nfile: \"lib.rs\"\nline: 3\ndegree: 4\ntags: [hg/node, crate/test, lang/rust, community/2]\n---\n\n# generated\n\n> `lib.rs:3` · crate `test` · degree 4\n\n## Links\n";
        assert!(super::parse_generated_vault_node_note(valid).is_some());

        for invalid in [
            valid.replacen("crate: test\nlang: rust", "lang: rust\ncrate: test", 1),
            valid.replacen("line: 3", "line: many", 1),
            valid.replacen("line: 3", "line: 03", 1),
            valid.replacen("degree: 4", "degree: many", 1),
            valid.replacen("tags: [hg/node,", "tags: [hg/notebook,", 1),
        ] {
            assert!(super::parse_generated_vault_node_note(&invalid).is_none());
        }
    }

    #[test]
    fn legacy_vault_moc_rejects_a_generated_note_subset() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn current() {}");
        fs::write(
            vault.path().join("stale.md"),
            legacy_vault_note(1, Some(0), "stale"),
        )
        .unwrap();
        fs::write(
            vault.path().join("current.md"),
            legacy_vault_note(2, Some(0), "current"),
        )
        .unwrap();
        let moc = "# Map of Content\n\n## community c0\n\n- [[current]]\n";
        fs::write(vault.path().join("_MOC.md"), moc).unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 4);
        assert!(vault.path().join("stale.md").exists());
        assert!(vault.path().join("current.md").exists());
        assert_eq!(
            fs::read_to_string(vault.path().join("_MOC.md")).unwrap(),
            moc
        );
        assert!(!vault.path().join(super::VAULT_MANIFEST).exists());
    }

    #[test]
    fn legacy_empty_vault_moc_is_not_claimed_with_stale_notes() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        fs::write(
            vault.path().join("stale.md"),
            legacy_vault_note(1, None, "stale"),
        )
        .unwrap();
        let moc = "# Map of Content\n";
        fs::write(vault.path().join("_MOC.md"), moc).unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 4);
        assert!(vault.path().join("stale.md").exists());
        assert_eq!(
            fs::read_to_string(vault.path().join("_MOC.md")).unwrap(),
            moc
        );
        assert!(!vault.path().join(super::VAULT_MANIFEST).exists());
    }

    #[test]
    fn empty_legacy_vault_moc_requires_generated_note_corroboration() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let vault = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        let user_moc = "# Map of Content\n";
        fs::write(vault.path().join("_MOC.md"), user_moc).unwrap();

        assert_eq!(run(src.path(), out.path(), Some(vault.path())), 4);
        assert_eq!(
            fs::read_to_string(vault.path().join("_MOC.md")).unwrap(),
            user_moc
        );
        assert!(!vault.path().join(super::VAULT_MANIFEST).exists());
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
    fn vault_sync_preflights_all_destinations_before_mutating() {
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
        assert!(!vault.path().join("written.md").exists());
        let journal = fs::read_to_string(vault.path().join(super::VAULT_MANIFEST)).unwrap();
        assert!(journal.contains(legacy_secret));

        fs::remove_dir(vault.path().join("blocked.md")).unwrap();
        sync_generated_vault(vault.path(), &rendered).unwrap();
        assert!(!vault.path().join(legacy_secret).exists());
        assert_eq!(
            fs::read_to_string(vault.path().join("written.md")).unwrap(),
            "written"
        );
        assert_eq!(
            fs::read_to_string(vault.path().join("blocked.md")).unwrap(),
            "unblocked"
        );
    }

    #[test]
    fn vault_sync_journals_pending_ownership_before_stale_removal() {
        let vault = TempDir::new().unwrap();
        let prior_owned =
            std::collections::HashSet::from(["stale.md".to_owned(), "blocked.md".to_owned()]);
        write_vault_manifest(vault.path(), &prior_owned).unwrap();
        fs::write(vault.path().join("stale.md"), "legacy").unwrap();
        fs::create_dir(vault.path().join("blocked.md")).unwrap();
        let rendered = vec![("current.md".to_owned(), "current".to_owned())];

        assert!(sync_generated_vault(vault.path(), &rendered).is_err());
        let pending = fs::read_to_string(vault.path().join(super::VAULT_MANIFEST)).unwrap();
        for filename in ["stale.md", "blocked.md"] {
            assert!(pending.contains(filename));
        }
        assert!(!pending.contains("current.md"));

        fs::remove_dir(vault.path().join("blocked.md")).unwrap();
        sync_generated_vault(vault.path(), &rendered).unwrap();
        assert!(!vault.path().join("stale.md").exists());
        assert_eq!(
            fs::read_to_string(vault.path().join("current.md")).unwrap(),
            "current"
        );
        let committed = fs::read_to_string(vault.path().join(super::VAULT_MANIFEST)).unwrap();
        assert!(committed.contains("current.md"));
        assert!(!committed.contains("stale.md"));
        assert!(!committed.contains("blocked.md"));
    }

    #[test]
    fn vault_pending_creation_requires_the_recorded_content() {
        let vault = TempDir::new().unwrap();
        let expected = "generated note";
        let pending = std::collections::BTreeMap::from([(
            "new.md".to_owned(),
            super::content_generation(expected.as_bytes()),
        )]);
        super::write_vault_manifest_state(
            vault.path(),
            &std::collections::HashSet::new(),
            &pending,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        fs::write(vault.path().join("new.md"), "user note").unwrap();
        let rendered = vec![("new.md".to_owned(), expected.to_owned())];

        assert!(sync_generated_vault(vault.path(), &rendered).is_err());
        assert_eq!(
            fs::read_to_string(vault.path().join("new.md")).unwrap(),
            "user note"
        );

        fs::write(vault.path().join("new.md"), expected).unwrap();
        sync_generated_vault(vault.path(), &rendered).unwrap();
    }

    #[test]
    fn vault_pending_deletion_requires_the_recorded_content() {
        let vault = TempDir::new().unwrap();
        let stale = "generated stale note";
        fs::write(vault.path().join("stale.md"), stale).unwrap();
        let deleting = std::collections::BTreeMap::from([(
            "stale.md".to_owned(),
            super::content_generation(stale.as_bytes()),
        )]);
        super::write_vault_manifest_state(
            vault.path(),
            &std::collections::HashSet::new(),
            &std::collections::BTreeMap::new(),
            &deleting,
        )
        .unwrap();
        fs::remove_file(vault.path().join("stale.md")).unwrap();
        fs::write(vault.path().join("stale.md"), "user replacement").unwrap();

        sync_generated_vault(
            vault.path(),
            &[("current.md".to_owned(), "current".to_owned())],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(vault.path().join("stale.md")).unwrap(),
            "user replacement"
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

    #[cfg(unix)]
    #[test]
    fn manifest_writes_do_not_follow_predictable_temporary_symlinks() {
        use std::os::unix::fs::symlink;

        let vault = TempDir::new().unwrap();
        let vault_victim = vault.path().join("vault-victim");
        fs::write(&vault_victim, "vault sentinel").unwrap();
        let vault_temporary = vault.path().join(format!(
            ".{}.tmp.{}",
            super::VAULT_MANIFEST,
            std::process::id()
        ));
        symlink(&vault_victim, &vault_temporary).unwrap();
        write_vault_manifest(vault.path(), &std::collections::HashSet::new()).unwrap();
        assert_eq!(fs::read_to_string(&vault_victim).unwrap(), "vault sentinel");

        let wiki = TempDir::new().unwrap();
        let wiki_victim = wiki.path().join("wiki-victim");
        fs::write(&wiki_victim, "wiki sentinel").unwrap();
        let wiki_temporary = wiki.path().join(format!(
            ".{}.tmp.{}",
            super::WIKI_MANIFEST,
            std::process::id()
        ));
        symlink(&wiki_victim, &wiki_temporary).unwrap();
        write_wiki_manifest(wiki.path(), &std::collections::HashSet::new()).unwrap();
        assert_eq!(fs::read_to_string(&wiki_victim).unwrap(), "wiki sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn generated_note_sync_refuses_symlink_destinations() {
        use std::os::unix::fs::symlink;

        let victim_dir = TempDir::new().unwrap();
        let vault_victim = victim_dir.path().join("vault-victim.md");
        fs::write(&vault_victim, "vault sentinel").unwrap();
        let vault = TempDir::new().unwrap();
        write_vault_manifest(
            vault.path(),
            &std::collections::HashSet::from(["owned.md".to_owned()]),
        )
        .unwrap();
        symlink(&vault_victim, vault.path().join("owned.md")).unwrap();
        let vault_rendered = vec![("owned.md".to_owned(), "replacement".to_owned())];
        assert!(sync_generated_vault(vault.path(), &vault_rendered).is_err());
        assert_eq!(fs::read_to_string(&vault_victim).unwrap(), "vault sentinel");

        let dangling_target = victim_dir.path().join("missing.md");
        let dangling_vault = TempDir::new().unwrap();
        symlink(&dangling_target, dangling_vault.path().join("new.md")).unwrap();
        let dangling_rendered = vec![("new.md".to_owned(), "replacement".to_owned())];
        assert!(sync_generated_vault(dangling_vault.path(), &dangling_rendered).is_err());
        assert!(!dangling_target.exists());

        let wiki_victim = victim_dir.path().join("wiki-victim.md");
        fs::write(&wiki_victim, "wiki sentinel").unwrap();
        let wiki = TempDir::new().unwrap();
        write_wiki_manifest(
            wiki.path(),
            &std::collections::HashSet::from(["index.md".to_owned()]),
        )
        .unwrap();
        symlink(&wiki_victim, wiki.path().join("index.md")).unwrap();
        let wiki_rendered = vec![("index.md".to_owned(), "replacement".to_owned())];
        assert!(sync_generated_wiki(wiki.path(), &wiki_rendered, false).is_err());
        assert_eq!(fs::read_to_string(&wiki_victim).unwrap(), "wiki sentinel");
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
    fn wiki_flag_refuses_to_claim_unsigned_generated_names() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        let wiki = out.path().join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(wiki.join("index.md"), "user index").unwrap();
        fs::write(wiki.join("node-1.md"), "user node").unwrap();
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };

        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 4);
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
    fn wiki_flag_migrates_strict_legacy_generated_pages() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let raw_label = "api_key_assignment_refused";
        mk_file(src.path(), "lib.rs", &format!("fn {raw_label}() {{}}"));
        let node_id = habitat_graph_core::content_id(raw_label);
        let wiki = out.path().join("wiki");
        fs::create_dir(&wiki).unwrap();
        fs::write(
            wiki.join("index.md"),
            format!("# Index\n\n- [{raw_label}](node-{node_id}.md)\n"),
        )
        .unwrap();
        fs::write(
            wiki.join(format!("node-{node_id}.md")),
            format!(
                "# {raw_label}\n\nSource: `lib.rs` line 1\n\n## Outbound\n\n_none_\n\n## Inbound\n\n_none_\n"
            ),
        )
        .unwrap();
        let unindexed_id = if node_id == u32::MAX {
            node_id - 1
        } else {
            node_id + 1
        };
        let unindexed = "# User article\n\nSource: `notes.md` line 1\n\n## Outbound\n\n_none_\n\n## Inbound\n\n_none_\n";
        fs::write(wiki.join(format!("node-{unindexed_id}.md")), unindexed).unwrap();
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };

        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        assert!(wiki.join(super::WIKI_MANIFEST).exists());
        for filename in ["index.md".to_owned(), format!("node-{node_id}.md")] {
            let content = fs::read_to_string(wiki.join(filename)).unwrap();
            assert!(content.contains(habitat_graph_export::wiki::GENERATED_WIKI_SIGNATURE));
            assert!(!content.contains(raw_label));
        }
        assert_eq!(
            fs::read_to_string(wiki.join(format!("node-{unindexed_id}.md"))).unwrap(),
            unindexed
        );
    }

    #[test]
    fn wiki_sync_journals_pending_ownership_before_stale_removal() {
        let wiki = TempDir::new().unwrap();
        let prior_owned =
            std::collections::HashSet::from(["node-1.md".to_owned(), "node-2.md".to_owned()]);
        write_wiki_manifest(wiki.path(), &prior_owned).unwrap();
        fs::write(wiki.path().join("node-1.md"), "legacy").unwrap();
        fs::create_dir(wiki.path().join("node-2.md")).unwrap();
        let rendered = vec![("index.md".to_owned(), "current".to_owned())];

        assert!(sync_generated_wiki(wiki.path(), &rendered, false).is_err());
        let pending = fs::read_to_string(wiki.path().join(super::WIKI_MANIFEST)).unwrap();
        for filename in ["node-1.md", "node-2.md"] {
            assert!(pending.contains(filename));
        }
        assert!(!pending.contains("index.md"));

        fs::remove_dir(wiki.path().join("node-2.md")).unwrap();
        sync_generated_wiki(wiki.path(), &rendered, false).unwrap();
        assert!(!wiki.path().join("node-1.md").exists());
        assert_eq!(
            fs::read_to_string(wiki.path().join("index.md")).unwrap(),
            "current"
        );
        let committed = fs::read_to_string(wiki.path().join(super::WIKI_MANIFEST)).unwrap();
        assert!(committed.contains("index.md"));
        assert!(!committed.contains("node-1.md"));
        assert!(!committed.contains("node-2.md"));
    }

    #[test]
    fn wiki_pending_creation_requires_the_recorded_content() {
        let wiki = TempDir::new().unwrap();
        let expected = "generated page";
        let pending = std::collections::BTreeMap::from([(
            "index.md".to_owned(),
            super::content_generation(expected.as_bytes()),
        )]);
        super::write_wiki_manifest_state(
            wiki.path(),
            &std::collections::HashSet::new(),
            &pending,
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        fs::write(wiki.path().join("index.md"), "user page").unwrap();
        let rendered = vec![("index.md".to_owned(), expected.to_owned())];

        assert!(sync_generated_wiki(wiki.path(), &rendered, false).is_err());
        assert_eq!(
            fs::read_to_string(wiki.path().join("index.md")).unwrap(),
            "user page"
        );

        fs::write(wiki.path().join("index.md"), expected).unwrap();
        sync_generated_wiki(wiki.path(), &rendered, false).unwrap();
    }

    #[test]
    fn wiki_pending_deletion_requires_the_recorded_content() {
        let wiki = TempDir::new().unwrap();
        let stale = "generated stale page";
        fs::write(wiki.path().join("node-1.md"), stale).unwrap();
        let deleting = std::collections::BTreeMap::from([(
            "node-1.md".to_owned(),
            super::content_generation(stale.as_bytes()),
        )]);
        super::write_wiki_manifest_state(
            wiki.path(),
            &std::collections::HashSet::new(),
            &std::collections::BTreeMap::new(),
            &deleting,
        )
        .unwrap();
        fs::remove_file(wiki.path().join("node-1.md")).unwrap();
        fs::write(wiki.path().join("node-1.md"), "user replacement").unwrap();

        sync_generated_wiki(
            wiki.path(),
            &[("index.md".to_owned(), "current".to_owned())],
            false,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(wiki.path().join("node-1.md")).unwrap(),
            "user replacement"
        );
    }

    #[test]
    fn wiki_flag_recovers_signed_pages_when_the_manifest_is_missing() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        let opts = ExtractOpts {
            wiki: true,
            ..ExtractOpts::default()
        };
        mk_file(src.path(), "lib.rs", "fn old_generated() {}");
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        let wiki = out.path().join("wiki");
        fs::remove_file(wiki.join(super::WIKI_MANIFEST)).unwrap();
        let old_pages: std::collections::HashSet<String> = fs::read_dir(&wiki)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|filename| filename.starts_with("node-"))
            .collect();

        mk_file(src.path(), "lib.rs", "fn new_generated() {}");
        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        assert!(old_pages
            .iter()
            .all(|filename| !wiki.join(filename).exists()));
        assert!(wiki.join(super::WIKI_MANIFEST).exists());
        for entry in fs::read_dir(&wiki).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == super::WIKI_MANIFEST {
                continue;
            }
            let content = fs::read_to_string(entry.path()).unwrap();
            assert!(content.contains(habitat_graph_export::wiki::GENERATED_WIKI_SIGNATURE));
        }
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
        assert!(out.path().join(super::OPTIONAL_ARTIFACT_MANIFEST).exists());

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
    fn default_run_preserves_unowned_optional_public_artifacts() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            fs::write(out.path().join(artifact), format!("user-owned {artifact}")).unwrap();
        }

        assert_eq!(run(src.path(), out.path(), None), 0);
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            assert_eq!(
                fs::read_to_string(out.path().join(artifact)).unwrap(),
                format!("user-owned {artifact}")
            );
        }
        assert!(!out.path().join(super::OPTIONAL_ARTIFACT_MANIFEST).exists());
    }

    #[test]
    fn explicit_flags_adopt_pre_manifest_optional_public_artifacts() {
        let src = TempDir::new().unwrap();
        let out = TempDir::new().unwrap();
        mk_file(src.path(), "lib.rs", "fn generated() {}");
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            fs::write(out.path().join(artifact), "legacy generated content").unwrap();
        }
        let opts = ExtractOpts {
            svg: true,
            graphml: true,
            neo4j: true,
            wiki: false,
        };

        assert_eq!(run_artifacts(src.path(), out.path(), None, opts), 0);
        for artifact in ["graph.svg", "graph.graphml", "graph.cypher"] {
            assert_ne!(
                fs::read_to_string(out.path().join(artifact)).unwrap(),
                "legacy generated content"
            );
        }
        assert!(out.path().join(super::OPTIONAL_ARTIFACT_MANIFEST).exists());
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
