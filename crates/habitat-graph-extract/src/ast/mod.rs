//! Per-language tree-sitter AST extractors.
//!
//! `rust` and `python` are unconditional (the proven parity baseline). The PA-1 grammars
//! (`ts`/`js`/`go`/`text`, S1008901) are feature-gated per R9a — a build compiles only the
//! languages whose features are enabled. Shared tree-sitter helpers live in [`util`].

pub mod python;
pub mod rust;

#[cfg(any(
    feature = "ts",
    feature = "js",
    feature = "go",
    feature = "text",
    feature = "java",
    feature = "ruby",
    feature = "c",
    feature = "cpp",
    feature = "scala",
    feature = "csharp",
    feature = "kotlin",
    feature = "php"
))]
pub mod util;

#[cfg(feature = "ts")]
pub mod ts;

#[cfg(feature = "js")]
pub mod js;

#[cfg(feature = "go")]
pub mod go;

#[cfg(feature = "text")]
pub mod text;

#[cfg(feature = "c")]
pub mod c;

#[cfg(feature = "cpp")]
pub mod cpp;

#[cfg(feature = "java")]
pub mod java;

#[cfg(feature = "ruby")]
pub mod ruby;

#[cfg(feature = "scala")]
pub mod scala;

#[cfg(feature = "csharp")]
pub mod csharp;

#[cfg(feature = "kotlin")]
pub mod kotlin;

#[cfg(feature = "php")]
pub mod php;
