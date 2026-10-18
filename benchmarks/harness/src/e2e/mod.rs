//! `lumen-bench e2e ...`: the end-to-end runs (E1-E3).
//!
//! `main.rs` dispatches here and nowhere else, so the tasks that fill this in replace
//! [`entry`] and add sibling modules without editing the dispatch.

use crate::util::Res;

pub const HELP: &str = "\
usage: benchmarks/run.sh e2e [options]

  the end-to-end runs (E1-E3). Not yet implemented.
";

/// Entry point for `lumen-bench e2e <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    Err("e2e: not yet implemented".to_string())
}
