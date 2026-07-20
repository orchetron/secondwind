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

    match reencode(&encoded.decoded) {
        Some(again) if again.wire == encoded.wire => {}
        _ => return Err(Refusal::NotIdempotent),
    }

    Ok(Certificate {
        clmh_before,
        clmh_after,
        canonical_bytes: canonical.len(),
        wire_bytes: encoded.wire.len(),
    })
}
