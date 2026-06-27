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
    /// Extract a knowledge graph from a directory of Rust source.
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
    /// Exercise the engine on a tiny in-memory corpus (no I/O).
    SelfTest,
    /// Print environment + wiring diagnostics.
    Doctor,
}

impl Cli {
    /// Runs the selected command and returns a process exit code.
    #[must_use]
    pub fn run(self) -> u8 {
        match self.command {
            Command::Extract { dir, out, vault } => {
                commands::extract::run(&dir, &out, vault.as_deref())
            }
            Command::Query { query, graph } => commands::query::run_query(&graph, &query),
            Command::Path { from, to, graph } => commands::query::run_path(&graph, &from, &to),
            Command::Serve { graph, addr } => commands::serve::run(&graph, &addr),
            Command::Mcp { graph } => commands::mcp::run(&graph),
            Command::SelfTest => commands::meta::self_test(),
            Command::Doctor => commands::meta::doctor(),
        }
    }
}
