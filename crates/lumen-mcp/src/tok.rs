// lumen-tok — reads text from stdin, prints exact BPE token count to stdout.
// Used by hook scripts for honest, non-estimated token metering.
//
// Usage: cat file.rs | lumen-tok
//        echo "some text" | lumen-tok
//
// Exit codes:
//   0  a count was written to stdout
//   3  the input is not text, so no count exists (nothing on stdout)
//   1  stdin could not be read at all
use std::io::Read;

/// Distinct from a generic failure so the caller can tell "I cannot measure this"
/// from "something went wrong". The meter uses it to record the read with a
/// provenance of `unsupported` instead of substituting bytes/4.
const EXIT_NOT_TEXT: i32 = 3;

fn main() {
    // Bytes, not read_to_string. read_to_string errors on the first invalid UTF-8
    // sequence and the previous `.expect()` turned that into a panic, so a binary
    // file made lumen-tok abort with a nonzero status. The meter read that as "the
    // tokenizer is broken" and fell back to bytes/4, which for a PNG overstates the
    // real cost by ~40x: three screenshots were recorded as 119,921 tokens against
    // roughly 2,750 actual. A tokenizer cannot count tokens in a PNG, and saying so
    // is the correct answer.
    let mut bytes = Vec::new();
    if std::io::stdin().lock().read_to_end(&mut bytes).is_err() {
        eprintln!("lumen-tok: cannot read stdin");
        std::process::exit(1);
    }

    match std::str::from_utf8(&bytes) {
        Ok(text) => println!("{}", lumen_core::tokenizer::count_tokens(text)),
        Err(_) => {
            eprintln!("lumen-tok: input is not valid UTF-8; no token count exists for it");
            std::process::exit(EXIT_NOT_TEXT);
        }
    }
}
