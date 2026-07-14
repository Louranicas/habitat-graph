//! Command-line interface definition (clap).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::commands;

/// Queryable knowledge-graph organ for the ULTRAPLATE factory.
#[derive(Parser, Debug)]
#[command(name = "habitat-graph", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Extract a public, deterministically redacted knowledge graph from a source directory.
    Extract {
        /// Root directory to scan.
        dir: PathBuf,
        /// Output directory for `graph.json` + `GRAPH_REPORT.md` + `graph.html`.
        #[arg(long, default_value = "graphify-out")]
        out: PathBuf,
        /// Also emit an Obsidian vault (one note per node, `[[wikilinks]]` + frontmatter/tags) into
        /// this directory — open it with Obsidian's graph view for interactive interconnection.
        #[arg(long)]
        vault: Option<PathBuf>,
        /// Also emit/adopt `graph.svg` (later runs refresh it from its ownership manifest).
        #[arg(long)]
        svg: bool,
        /// Also emit/adopt `graph.graphml` (later runs refresh it from its ownership manifest).
        #[arg(long)]
        graphml: bool,
        /// Also emit/adopt `graph.cypher` (later runs refresh it from its ownership manifest).
        #[arg(long)]
        neo4j: bool,
        /// Also emit/adopt a generated `wiki/` (unowned pages are never overwritten).
        #[arg(long)]
        wiki: bool,
    },
    /// Incrementally rebuild using owner-only raw state and refresh public redacted artifacts.
    Update {
        /// Root directory to scan.
        dir: PathBuf,
        /// Output directory holding the existing `graph.json` to update.
        #[arg(long, default_value = "graphify-out")]
        out: PathBuf,
    },
    /// Search nodes whose label contains a substring (case-insensitive).
    Query {
        /// Substring to search for.
        query: String,
        /// Path to a node-link `graph.json`.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
    },
    /// Find the shortest path between two node labels.
    Path {
        /// Source node label.
        from: String,
        /// Target node label.
        to: String,
        /// Path to a node-link `graph.json`.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
    },
    /// Run the HTTP service (`/health`, `/query`, `/path`) over a graph until interrupted.
    Serve {
        /// Path to a node-link `graph.json`.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
        /// Address to bind (host:port).
        #[arg(long, default_value = "127.0.0.1:7878")]
        addr: String,
    },
    /// Serve the graph over the Model Context Protocol (JSON-RPC on stdio).
    Mcp {
        /// Path to a node-link `graph.json`.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
    },
    /// Git merge driver preserving redacted IDs/provenance (`merge-driver %O %A %B`).
    MergeDriver {
        /// Base (`%O`) — the merge ancestor `graph.json`.
        base: PathBuf,
        /// Ours (`%A`) — our `graph.json`; the deterministic merge is written back here.
        ours: PathBuf,
        /// Theirs (`%B`) — the other side's `graph.json`.
        theirs: PathBuf,
    },
    /// Register the deterministic `graph.json` merge driver in a repository.
    InstallMergeDriver {
        /// Repository root to install into (writes `.gitattributes`).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Register habitat-graph as an MCP server (prints the config, or merges with `--write`).
    InstallMcp {
        /// Path to the `graph.json` the MCP server should serve.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
        /// Server name to register under `mcpServers`.
        #[arg(long, default_value = commands::install_mcp::DEFAULT_SERVER_NAME)]
        name: String,
        /// Merge into this config file instead of printing the block to stdout.
        #[arg(long)]
        write: Option<PathBuf>,
    },
    /// Auto-discover the Claude MCP config and register habitat-graph (snapshot-write-readback safe).
    Install {
        /// Path to the `graph.json` the MCP server should serve.
        #[arg(long, default_value = "graphify-out/graph.json")]
        graph: PathBuf,
        /// Print what would be written without touching disk.
        #[arg(long)]
        dry_run: bool,
        /// Override the config path instead of auto-discovering it.
        #[arg(long)]
        config_path: Option<PathBuf>,
    },
    /// Install git lifecycle hooks (post-commit rebuild and/or merge driver).
    Hook {
        #[command(subcommand)]
        action: HookAction,
    },
    /// Watch source files for changes and rebuild the graph automatically (requires --features watch).
    Watch {
        /// Root directory to watch.
        dir: PathBuf,
        /// Output directory for `graph.json`.
        #[arg(long, default_value = commands::watch::DEFAULT_OUT)]
        out: PathBuf,
    },
    /// Fetch a remote source without redirects and merge it (requires --features live).
    Add {
        /// Public HTTP(S) URL to fetch; private/internal targets and redirects are refused.
        url: String,
        /// Output `graph.json` to merge the new nodes into.
        #[arg(long, default_value = "graphify-out/graph.json")]
        out: PathBuf,
    },
    /// Exercise the engine on a tiny in-memory corpus (no I/O).
    SelfTest,
    /// Print environment + wiring diagnostics.
    Doctor,
}

/// Sub-actions for the `hook` subcommand.
#[derive(Subcommand, Debug)]
pub enum HookAction {
    /// Install a post-commit hook that regenerates the graph after every commit.
    Install {
        /// Directory to start the git repo search from.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
    /// Register the habitat-graph merge driver in `.git/config` and `.gitattributes`.
    InstallMergeDriver {
        /// Directory to start the git repo search from.
        #[arg(long, default_value = ".")]
        dir: PathBuf,
    },
}

impl Cli {
    /// Runs the selected command and returns a process exit code.
    #[must_use]
    pub fn run(self) -> u8 {
        match self.command {
            Command::Extract {
                dir,
                out,
                vault,
                svg,
                graphml,
                neo4j,
                wiki,
            } => {
                // Extra exporters requested → full path; otherwise the canonical no-opt `run`.
                if svg || graphml || neo4j || wiki {
                    let opts = commands::extract::ExtractOpts {
                        svg,
                        graphml,
                        neo4j,
                        wiki,
                    };
                    commands::extract::run_artifacts(&dir, &out, vault.as_deref(), opts)
                } else {
                    commands::extract::run(&dir, &out, vault.as_deref())
                }
            }
            Command::Update { dir, out } => commands::update::run(&dir, &out),
            Command::Query { query, graph } => commands::query::run_query(&graph, &query),
            Command::Path { from, to, graph } => commands::query::run_path(&graph, &from, &to),
            Command::Serve { graph, addr } => commands::serve::run(&graph, &addr),
            Command::Mcp { graph } => commands::mcp::run(&graph),
            Command::MergeDriver { base, ours, theirs } => {
                commands::merge_driver::run_merge_driver(&base, &ours, &theirs)
            }
            Command::InstallMergeDriver { repo } => commands::merge_driver::install(&repo),
            Command::InstallMcp { graph, name, write } => {
                commands::install_mcp::run(&graph, &name, write.as_deref())
            }
            Command::Install {
                graph,
                dry_run,
                config_path,
            } => commands::install::run(&graph, config_path.as_deref(), dry_run),
            Command::Hook { action } => match action {
                HookAction::Install { dir } => commands::hook::run_install(&dir),
                HookAction::InstallMergeDriver { dir } => {
                    commands::hook::run_install_merge_driver(&dir)
                }
            },
            Command::Watch { dir, out } => commands::watch::run(&dir, &out),
            Command::Add { url, out } => commands::add::run(&url, &out),
            Command::SelfTest => commands::meta::self_test(),
            Command::Doctor => commands::meta::doctor(),
        }
    }
}
