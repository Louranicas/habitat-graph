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
        /// Output directory for `graph.json` + `GRAPH_REPORT.md`.
        #[arg(long, default_value = "graphify-out")]
        out: PathBuf,
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
            Command::Extract { dir, out } => commands::extract::run(&dir, &out),
            Command::SelfTest => commands::meta::self_test(),
            Command::Doctor => commands::meta::doctor(),
        }
    }
}
