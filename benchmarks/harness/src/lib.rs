//! `lumen-bench`: the benchmark harness for Lumen's tools and hooks.
//!
//! It drives the real `lumen-mcp` binary, counts with the real tokenizer, and measures two builds
//! over one pinned corpus. See `benchmarks/README.md` for the method and its limits.
//!
//! This crate is a development tool. No shipped binary depends on it, and it is not part of the
//! Tauri bundle.

pub mod assets;
pub mod builds;
pub mod compare;
pub mod competitors;
pub mod corpus;
pub mod driver;
pub mod e2e;
pub mod edit;
pub mod env;
pub mod latency;
pub mod manifest;
pub mod report;
pub mod run;
pub mod sandbox;
pub mod score;
pub mod stats;
pub mod tokens;
pub mod util;
pub mod versions;
