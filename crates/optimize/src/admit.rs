use serde_json::Value;

use crate::atom::{canonicalize, leaves};
use crate::clmh::Clmh;
use crate::transform::Encoded;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    ClmhMismatch,
    InverseWitnessFailed,
    NotIdempotent,
}

#[derive(Debug)]
pub struct Certificate {
    pub clmh_before: Clmh,
    pub clmh_after: Clmh,
    pub canonical_bytes: usize,
    pub wire_bytes: usize,
}

// The inverse witness is required on top of CLMH equality: multiset equality
// alone permits a value bound to the wrong key, which the byte check rejects.
pub fn admit(
    original: &Value,
    encoded: &Encoded,
    reencode: impl Fn(&Value) -> Option<Encoded>,
    verify_reencode: bool,
) -> Result<Certificate, Refusal> {
    let clmh_before = Clmh::of(&leaves(original));
    let clmh_after = Clmh::of(&leaves(&encoded.decoded));
    if clmh_before != clmh_after {
        return Err(Refusal::ClmhMismatch);
    }

    let canonical = canonicalize(original);
    if canonicalize(&encoded.decoded) != canonical {
        return Err(Refusal::InverseWitnessFailed);
    }

    // Idempotence: re-encoding the decoded value reproduces the wire. This proves the codec is a
    // stable fixed point, not that the block is lossless (CLMH + the inverse witness above already
    // do that). A trusted built-in codec is fuzzed for this and skips the re-encode; a host-supplied
    // codec keeps it, so a reckless round trip is still refused, never shipped.
    if verify_reencode {
        match reencode(&encoded.decoded) {
            Some(again) if again.wire == encoded.wire => {}
            _ => return Err(Refusal::NotIdempotent),
        }
    }

    Ok(Certificate {
        clmh_before,
        clmh_after,
        canonical_bytes: canonical.len(),
        wire_bytes: encoded.wire.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    #[test]
    fn trusted_codec_skips_the_idempotence_reencode_untrusted_keeps_it() {
        let value = json!([{"a": 1, "b": 2}, {"a": 3, "b": 4}]);
        let encoded = Encoded {
            wire: "wire".into(),
            decoded: value.clone(),
        };
        let calls = Cell::new(0usize);
        let reencode = |v: &Value| {
            calls.set(calls.get() + 1);
            Some(Encoded {
                wire: "wire".into(),
                decoded: v.clone(),
            })
        };

        // Untrusted host codec: the idempotence re-encode runs (a reckless round trip is still caught).
        calls.set(0);
        assert!(admit(&value, &encoded, reencode, true).is_ok());
        assert_eq!(calls.get(), 1, "untrusted codec re-encodes for the check");

        // Trusted built-in: the re-encode is skipped, and admission still succeeds because CLMH and
        // the inverse witness (which prove losslessness) run either way.
        calls.set(0);
        assert!(admit(&value, &encoded, reencode, false).is_ok());
        assert_eq!(calls.get(), 0, "trusted codec skips the re-encode");
    }
}
