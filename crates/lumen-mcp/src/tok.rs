// lumen-tok — reads text from stdin, prints exact BPE token count to stdout.
// Used by hook scripts for honest, non-estimated token metering.
//
// Usage: cat file.rs | lumen-tok
//        echo "some text" | lumen-tok
use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin()
        .lock()
        .read_to_string(&mut text)
        .expect("failed to read stdin");
    println!("{}", lumen_core::tokenizer::count_tokens(&text));
}
