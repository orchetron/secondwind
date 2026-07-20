use std::collections::HashMap;

use crate::atom::Atom;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clmh([u8; 32]);

impl Clmh {
    pub fn of(atoms: &[Atom]) -> Self {
        let mut counts: HashMap<[u8; 32], u32> = HashMap::new();
        let mut acc = [0u8; 32];
        for atom in atoms {
            let fp = atom.fingerprint();
            // Occurrence index folds multiplicity in: [a, a] and [a] differ.
            let occurrence = counts.entry(fp).or_insert(0);
            let mut hasher = blake3::Hasher::new();
            hasher.update(&fp);
            hasher.update(&occurrence.to_le_bytes());
            let term = hasher.finalize();
            for (a, t) in acc.iter_mut().zip(term.as_bytes()) {
                *a ^= *t;
            }
            *occurrence += 1;
        }
        Clmh(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::AtomClass;

    fn num(bytes: &str) -> Atom {
        Atom {
            class: AtomClass::Number,
            bytes: bytes.into(),
        }
    }

    #[test]
    fn order_independent() {
        let a = [num("1"), num("2"), num("3")];
        let b = [num("3"), num("1"), num("2")];
        assert_eq!(Clmh::of(&a), Clmh::of(&b));
    }

    #[test]
    fn multiplicity_matters() {
        let one = [num("1")];
        let two = [num("1"), num("1")];
        assert_ne!(Clmh::of(&one), Clmh::of(&two));
    }

    #[test]
    fn any_mutation_changes_the_hash() {
        let base = [num("1"), num("2")];
        let mutated = [num("1"), num("3")];
        let dropped = [num("1")];
        let added = [num("1"), num("2"), num("2")];
        assert_ne!(Clmh::of(&base), Clmh::of(&mutated));
        assert_ne!(Clmh::of(&base), Clmh::of(&dropped));
        assert_ne!(Clmh::of(&base), Clmh::of(&added));
    }

    #[test]
    fn class_matters() {
        let as_number = [num("1")];
        let as_text = [Atom {
            class: AtomClass::Text,
            bytes: "1".into(),
        }];
        assert_ne!(Clmh::of(&as_number), Clmh::of(&as_text));
    }
}
