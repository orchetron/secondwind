// Lossy AST-style skeletonization vs lossless code offload on this crate's own source: both hand
// the model signatures, only offload keeps the full body recoverable. Run:
//   cargo run -p secondwind-optimize --example code_vs_skeleton --features tiktoken
use secondwind_optimize::offload::Store;
use secondwind_optimize::outline;
use secondwind_optimize::tokens::{Tiktoken, TokenCounter};

const FILES: &[(&str, &str)] = &[
    ("proxy.rs", include_str!("../src/proxy.rs")),
    ("lib.rs", include_str!("../src/lib.rs")),
    ("offload.rs", include_str!("../src/offload.rs")),
    ("outline.rs", include_str!("../src/outline.rs")),
];

// AST-skeleton stand-in: keep signature lines, collapse each body to "...". Lossy: bodies unrecoverable.
fn skeleton(code: &str) -> String {
    const LEADS: &[&str] = &[
        "fn ",
        "def ",
        "func ",
        "function ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "interface ",
        "type ",
        "const ",
        "mod ",
        "use ",
        "import ",
        "from ",
        "#include",
        "namespace ",
        "package ",
        "let ",
        "var ",
        "val ",
    ];
    const MODS: &[&str] = &[
        "pub ",
        "pub(crate) ",
        "async ",
        "static ",
        "export ",
        "default ",
        "final ",
        "public ",
        "private ",
        "protected ",
        "unsafe ",
        "open ",
        "override ",
    ];
    let mut out = String::new();
    let mut collapsed = false;
    for line in code.lines() {
        let mut rest = line.trim_start();
        while let Some(m) = MODS.iter().find(|m| rest.starts_with(**m)) {
            rest = &rest[m.len()..];
        }
        if LEADS.iter().any(|l| rest.starts_with(l)) || line.trim_start().starts_with('}') {
            out.push_str(line);
            out.push('\n');
            collapsed = false;
        } else if !collapsed {
            let indent: String = line
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            out.push_str(&indent);
            out.push_str("...\n");
            collapsed = true;
        }
    }
    out
}

fn main() {
    let counter = Tiktoken::cl100k();
    let store = Store::default();
    println!(
        "{:<12} {:>9} {:>16} {:>18} {:>9}",
        "file", "orig", "skeleton", "secondwind", "recover"
    );
    println!(
        "{:<12} {:>9} {:>16} {:>18} {:>9}",
        "", "tokens", "tokens (lossy)", "tokens (lossless)", ""
    );
    println!("{}", "-".repeat(68));
    let (mut orig_sum, mut skel_sum, mut sw_sum) = (0usize, 0usize, 0usize);
    for (name, code) in FILES {
        let orig = counter.count(code);
        let skel = counter.count(&skeleton(code));
        let off = store.offload(code).expect("a source file offloads");
        let stub = counter.count(&off.stub);
        let recovered = store.resolve(&off.marker).as_deref() == Some(*code);
        assert!(
            recovered,
            "the offload must recover the exact original bytes"
        );
        println!("{name:<12} {orig:>9} {skel:>16} {stub:>18} {:>9}", "exact");
        orig_sum += orig;
        skel_sum += skel;
        sw_sum += stub;
    }
    println!("{}", "-".repeat(68));
    let pct = |x: usize| 100.0 * (orig_sum - x) as f64 / orig_sum as f64;
    println!(
        "totals  {orig_sum:>9} {:>16} {:>18} {:>9}",
        format!("{skel_sum} ({:.0}%)", pct(skel_sum)),
        format!("{sw_sum} ({:.0}%)", pct(sw_sum)),
        "exact",
    );
    println!(
        "\nskeleton: {:.0}% saved but LOSSY, the bodies are gone and unrecoverable.",
        pct(skel_sum)
    );
    println!(
        "secondwind: {:.0}% saved, LOSSLESS, resolve the marker for the exact body.",
        pct(sw_sum)
    );
    if let Some(o) = outline::outline(FILES[0].1, 300) {
        println!(
            "\nsignatures the model still sees in the {} stub:",
            FILES[0].0
        );
        for line in o.lines().take(5) {
            println!("  {line}");
        }
    }
}
