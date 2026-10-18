//! `lumen-bench competitors ...`: the competitor comparison (C1-C3).
//!
//! `main.rs` dispatches here and nowhere else, so the tasks that fill this in replace
//! [`entry`] and add sibling modules without editing the dispatch.

use crate::util::Res;

pub const HELP: &str = "\
usage: benchmarks/run.sh competitors [options]

  the competitor comparison (C1-C3). Not yet implemented.
";

/// Entry point for `lumen-bench competitors <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    Err("competitors: not yet implemented".to_string())
}
