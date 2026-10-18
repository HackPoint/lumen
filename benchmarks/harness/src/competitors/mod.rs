//! `lumen-bench competitors ...`: the competitor comparison (C1-C3).
//!
//! `main.rs` dispatches here and nowhere else, so the tasks that fill this in replace
//! [`entry`] and add sibling modules without editing the dispatch.

pub mod record;

use crate::util::Res;

pub const HELP: &str = "\
usage: benchmarks/run.sh competitors [options]

  the competitor comparison (C1-C3).

  record    record the Layer 1 corpus from pinned public sources (C1); `record --help`
";

/// Entry point for `lumen-bench competitors <args>`. `args` are what follows the subcommand.
pub fn entry(args: &[String]) -> Res<()> {
    let help = args.iter().any(|a| a == "--help" || a == "-h");
    if args.first().map(String::as_str) == Some("record") {
        if help {
            print!("{}", record::HELP);
            return Ok(());
        }
        return record::run(&args[1..]);
    }
    if help {
        print!("{HELP}");
        return Ok(());
    }
    Err("competitors: not yet implemented (only `record` so far)".to_string())
}
