//! `lumen-bench`: the benchmark harness for Lumen's tools and hooks.
//!
//! It drives the real `lumen-mcp` binary, counts with the real tokenizer, and measures two builds
//! over one pinned corpus. See `benchmarks/README.md` for the method and its limits.
//!
//! This crate is a development tool. No shipped binary depends on it, and it is not part of the
//! Tauri bundle.

pub mod builds;
pub mod compare;
pub mod corpus;
pub mod driver;
pub mod edit;
pub mod env;
pub mod latency;
pub mod report;
pub mod run;
pub mod stats;
pub mod tokens;
pub mod util;
