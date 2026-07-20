use serde_json::Value;

use crate::{atom, columnar, dict, lines, log, search, text_columnar};

// Portable fidelity proof: hash of the canonical original. Anyone can decode a wire,
// recompute the hash, and confirm losslessness without trusting the compressor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    pub hash: String,
}

pub fn certify(original: &str) -> Certificate {
    Certificate {
        hash: hash(&canonical(original)),
    }
}

// True when the wire decodes to content whose canonical hash matches the certificate.
// Offloaded blocks verify against their resolved body through the same hash.
pub fn verify(wire: &str, certificate: &Certificate) -> bool {
    reconstruct(wire).is_some_and(|canonical| hash(&canonical) == certificate.hash)
}

fn reconstruct(wire: &str) -> Option<String> {
    if let Some(value) = columnar::decode(wire) {
        return Some(atom::canonicalize(&value));
    }
    if let Some(text) = text_columnar::decode(wire) {
        return Some(canonical(&text));
    }
    if let Some(text) = search::decode(wire) {
        return Some(canonical(&text));
    }
    if let Some(text) = log::decode(wire) {
        return Some(canonical(&text));
    }
    if let Some(text) = dict::decode(wire) {
        return Some(canonical(&text));
    }
    if let Some(text) = lines::decode(wire) {
        return Some(canonical(&text));
    }
    None
}

fn canonical(text: &str) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => atom::canonicalize(&value),
        Err(_) => text.to_string(),
    }
}

fn hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columnar::Columnar;
    use crate::transform::Transform;

    fn array() -> String {
        let rows: Vec<String> = (0..30)
            .map(|i| format!(r#"{{"id":{i},"name":"user_{i}","role":"member"}}"#))
            .collect();
        format!("[{}]", rows.join(","))
    }

    #[test]
    fn a_text_columnar_wire_verifies_against_its_certificate() {
        let raw: String = (0..40)
            .map(|i| format!("-rw-r--r-- 1 root wheel {:>6} file-{i}.txt", 100 + i * 37))
            .collect::<Vec<_>>()
            .join("\n");
        let bytes = |s: &str| s.len();
        let encoded = text_columnar::try_encode(&raw, &bytes).expect("tabular block compresses");
        assert!(
            verify(&encoded.wire, &certify(&raw)),
            "the columns wire must verify lossless"
        );
    }

    #[test]
    fn a_lossless_wire_verifies_against_its_certificate() {
        let raw = array();
        let value: Value = serde_json::from_str(&raw).unwrap();
        let wire = Columnar::default().try_encode(&value).unwrap().wire;
        let certificate = certify(&raw);
        assert!(verify(&wire, &certificate));
    }

    #[test]
    fn a_wire_with_a_dropped_value_fails_verification() {
        let raw = array();
        let certificate = certify(&raw);
        // A tampered wire that decodes to a different value must not verify.
        let mut value: Value = serde_json::from_str(&raw).unwrap();
        value.as_array_mut().unwrap().truncate(29);
        let tampered = Columnar::default().try_encode(&value).unwrap().wire;
        assert!(!verify(&tampered, &certificate));
    }

    #[test]
    fn a_certificate_from_a_different_original_does_not_verify() {
        let wire = Columnar::default()
            .try_encode(&serde_json::from_str::<Value>(&array()).unwrap())
            .unwrap()
            .wire;
        assert!(!verify(&wire, &certify("[{\"id\":999}]")));
    }
}
