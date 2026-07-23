// Emit the offload content preview for a block, so a harness can check the model reads it.
use secondwind_optimize::offload::preview_if_offloaded;
use std::io::Read;
fn main() {
    let raw = match std::env::args().nth(1) {
        Some(p) => std::fs::read_to_string(p).unwrap(),
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).unwrap();
            s
        }
    };
    match preview_if_offloaded(&raw) {
        Some(p) => print!("{p}"),
        None => print!("{raw}"),
    }
}
