// Emit the inline-compressed (offload-disabled, lossless) wire for a block, so an external harness can
// feed the model exactly what secondwind would put in the window. Reads a file arg or stdin.
use secondwind_optimize::tokens::Tiktoken;
use secondwind_optimize::{Optimizer, Outcome};
use std::io::Read;
use std::sync::Arc;

fn main() {
    let raw = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(path).unwrap(),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).unwrap();
            s
        }
    };
    let mut opt = Optimizer::default().with_counter(Arc::new(Tiktoken::cl100k()));
    opt.set_offload_allowed(false);
    match opt.compress_block(&raw) {
        Outcome::Compressed { wire, .. } => print!("{wire}"),
        _ => print!("{raw}"),
    }
}
