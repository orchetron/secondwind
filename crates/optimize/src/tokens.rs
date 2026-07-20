// Codec selection counts through a TokenCounter (the billed unit is tokens, not bytes) so the byte
// proxy can be swapped for a real tokenizer without touching the transforms.
pub trait TokenCounter: Send + Sync {
    fn count(&self, text: &str) -> usize;
    // True when count returns real tokens, so the gate prices them directly, not as bytes.
    fn native(&self) -> bool {
        false
    }
}

// Default: byte length as a token proxy, no dependency. Keeps the byte path intact.
pub struct ByteCounter;

impl TokenCounter for ByteCounter {
    fn count(&self, text: &str) -> usize {
        text.len()
    }
}

// cl100k tokenizer via bpe-openai: counts byte-identical to tiktoken (see parity test) but faster,
// vocab is a process-wide static so construction is free. Name kept for call sites; counts matter, not backend.
#[cfg(feature = "tiktoken")]
pub struct Tiktoken {
    bpe: &'static bpe_openai::Tokenizer,
}

#[cfg(feature = "tiktoken")]
impl Tiktoken {
    pub fn cl100k() -> Self {
        Self {
            bpe: bpe_openai::cl100k_base(),
        }
    }
}

#[cfg(feature = "tiktoken")]
impl TokenCounter for Tiktoken {
    fn count(&self, text: &str) -> usize {
        self.bpe.count(text)
    }
    // Real tokens: the gate prices the billed unit directly, so figures match the dashboard's counts.
    fn native(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counter_counts_bytes() {
        assert_eq!(ByteCounter.count("hello"), 5);
    }

    // Swap gate: bpe-openai must match tiktoken-rs cl100k counts byte-for-byte or savings shift silently. Mismatch = do NOT swap.
    #[cfg(feature = "tiktoken")]
    #[test]
    fn bpe_openai_matches_tiktoken_cl100k_on_a_corpus() {
        let tik = tiktoken_rs::cl100k_base().unwrap();
        let bpe = bpe_openai::cl100k_base();
        let big_json = serde_json::to_string(
            &(0..60)
                .map(|i| serde_json::json!({"id": i, "name": format!("pod-{i}"), "ip": format!("10.0.0.{i}")}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let many_words = "word ".repeat(500);
        let corpus: &[&str] = &[
            "",
            "hello world",
            "The quick brown fox jumps over 13 lazy dogs.",
            r#"{"name":"pod-1","status":"Running","restarts":0,"ip":"10.0.0.1"}"#,
            "fn main() {\n    let x: Vec<u32> = (0..10).collect();\n}",
            "café résumé naïve — smart quotes \u{201c}q\u{201d} emoji 🚀🔥 CJK 日本語テキスト",
            "  \t leading   whitespace\n\n\nand blank lines\t\t end",
            "1234567890 3.14159 -42 0xDEADBEEF 1e10 v1.2.3-rc4",
            "-rwxr-xr-x  1 root  wheel  12345 Jan  1 12:00 filename.rs",
            &many_words,
            &big_json,
        ];
        for s in corpus {
            let a = tik.encode_ordinary(s).len();
            let b = bpe.count(s);
            assert_eq!(
                a,
                b,
                "cl100k count mismatch on {:?}: tiktoken={a} bpe-openai={b}",
                &s[..s.len().min(48)]
            );
        }
    }
}
