use once_cell::sync::Lazy;
use tiktoken_rs::{CoreBPE, cl100k_base};

static BPE: Lazy<CoreBPE> = Lazy::new(|| cl100k_base().expect("failed to load BPE model"));

// Count tokens in a piece of text (real BPE, not chars/4)
pub fn count_tokens(text: &str) -> usize {
    BPE.encode_with_special_tokens(text).len()
}
