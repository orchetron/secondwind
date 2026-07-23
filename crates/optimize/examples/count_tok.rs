// Count cl100k tokens of stdin, the exact tokenizer the proxy bills on.
use secondwind_optimize::Optimizer;
use secondwind_optimize::tokens::Tiktoken;
use std::io::Read;
use std::sync::Arc;
fn main() {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).unwrap();
    let opt = Optimizer::default().with_counter(Arc::new(Tiktoken::cl100k()));
    println!("{}", opt.count(&s));
}
