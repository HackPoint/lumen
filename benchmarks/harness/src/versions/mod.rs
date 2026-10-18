//! `lumen-bench versions ...`: the version-over-version benchmark (V2-V5).
//!
//! `main.rs` dispatches here and nowhere else, so the tasks that fill this in replace
//! [`entry`] and add sibling modules without editing the dispatch.

use crate::util::Res;

pub const HELP: &str = "\
usage: benchmarks/run.sh versions [options]

  the version-over-version benchmark (V2-V5). Not yet implemented.
";

/// Entry point for `lumen-bench versions <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return Ok(());
    }
    Err("versions: not yet implemented".to_string())
}
