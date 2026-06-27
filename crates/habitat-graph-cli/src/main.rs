//! `habitat-graph` CLI binary — extract a queryable knowledge graph from source.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod cli;
mod commands;

use clap::Parser;

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(cli::Cli::parse().run())
}
