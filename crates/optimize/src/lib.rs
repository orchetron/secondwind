#![forbid(unsafe_code)]

use serde_json::Value;

pub mod admit;
pub mod atom;
pub mod cachecost;
pub mod certificate;
pub mod clmh;
pub mod columnar;
pub mod counterfactual;
pub mod dedup;
pub mod detectorgate;
pub mod dict;
pub mod distilled;
pub mod frontier;
pub mod lines;
pub mod log;
pub mod netcost;
pub mod offload;
pub mod outline;
pub mod prefix;
pub mod prose;
pub mod proxy;
pub mod reconcile;
pub mod relevance;
pub mod replay;
pub mod resolve;
pub mod richness;
pub mod search;
pub mod shape;
pub mod text_columnar;
pub mod tokens;
pub mod transform;

pub use atom::canonicalize;

// Atom count + blake3 fidelity certificate of a raw block, so every surface records the same proof.
pub fn proof(raw: &str) -> (u64, String) {
    let atoms = serde_json::from_str::<Value>(raw)
        .map(|value| atom::leaves(&value).len() as u64)
        .unwrap_or_else(|_| raw.split_whitespace().count() as u64);
    (atoms, certificate::certify(raw).hash)
}

use std::sync::Arc;

use admit::{Certificate, Refusal, admit};
use columnar::Columnar;
use netcost::{NetCostGate, Verdict, Zone};
use offload::{OffloadStore, Store};
use tokens::{ByteCounter, TokenCounter};
use transform::{Encoded, TextProposer, Transform};

pub enum Outcome {
    Compressed {
        wire: String,
        transform: &'static str,
        certificate: Certificate,
        saved_usd: f64,
    },
    Offloaded {
        stub: String,
        marker: String,
        saved_usd: f64,
    },
    KeptVerbatim {
        reason: KeptReason,
    },
}

pub enum KeptReason {
    NotApplicable,
    Refused(&'static str, Refusal),
    NoNetSaving(netcost::Reason),
    DetectorFired,
}

impl KeptReason {
    // Stable slug for the ledger's refused/kept-by-reason breakdown.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeptReason::NotApplicable => "not_applicable",
            KeptReason::Refused(_, Refusal::ClmhMismatch) => "refused_clmh",
            KeptReason::Refused(_, Refusal::InverseWitnessFailed) => "refused_witness",
            KeptReason::Refused(_, Refusal::NotIdempotent) => "refused_idempotency",
            KeptReason::NoNetSaving(netcost::Reason::FrozenWriteWouldExceedRead) => {
                "no_saving_frozen"
            }
            KeptReason::NoNetSaving(netcost::Reason::NotWorthIt) => "no_saving_notworth",
            KeptReason::NoNetSaving(netcost::Reason::UnknownModel) => "no_saving_unknown_model",
            KeptReason::DetectorFired => "detector",
        }
    }
}

// Expected fraction of an offloaded block the agent pulls back via resolve; inline wins when its
// factored size beats stub + REOPEN_PRIOR * body. Measured 0.45 over 1055 real traces (2229 offloads).
const REOPEN_PRIOR: f64 = 0.45;

const RELEVANCE_MIN_ROWS: usize = 8;
const RELEVANCE_MAX_INLINE: usize = 24;

const PROSE_SUMMARY_MIN_BYTES: usize = 1000;
const PROSE_SUMMARY_FRACTION: f64 = 0.5;

pub struct Optimizer {
    transforms: Vec<Box<dyn Transform>>,
    // Text codecs searched best-of-N and proven per-instance (decode == raw) before a block falls
    // to offload. Built-ins and any host-registered codec share the same proof gate.
    text_proposers: Vec<Box<dyn TextProposer>>,
    proposers_enabled: bool,
    gate: NetCostGate,
    zone: Zone,
    store: Arc<dyn OffloadStore>,
    counter: Arc<dyn TokenCounter>,
    embedder: Arc<dyn relevance::Embedder>,
    prose_mode: bool,
    prose_shrinker: Option<Arc<dyn prose::ProseShrinker>>,
    offload_allowed: bool,
}

impl Default for Optimizer {
    fn default() -> Self {
        Self::new(NetCostGate::new("claude-sonnet-4-5", 1), Zone::Suffix)
    }
}

impl Optimizer {
    pub fn new(gate: NetCostGate, zone: Zone) -> Self {
        Self {
            transforms: vec![Box::new(Columnar::default())],
            text_proposers: vec![Box::new(dict::Dict), Box::new(lines::Lines)],
            proposers_enabled: true,
            gate,
            zone,
            store: Arc::new(Store::default()),
            counter: Arc::new(ByteCounter),
            embedder: Arc::new(distilled::DistilledEmbedder),
            prose_mode: false,
            prose_shrinker: None,
            offload_allowed: true,
        }
    }

    // False when the request carries no resolver: prefer inline lossless codecs over an offload that
    // could never be surfaced, so a resolver-less agent still gets same-turn compression.
    pub fn set_offload_allowed(&mut self, allowed: bool) {
        self.offload_allowed = allowed;
    }

    // Swap the baked-in relevance embedder for a stronger backend.
    pub fn with_embedder(mut self, embedder: Arc<dyn relevance::Embedder>) -> Self {
        self.embedder = embedder;
        self
    }

    // The model whose rates price every saving.
    pub fn model(&self) -> &str {
        self.gate.model()
    }

    // Price against the given model. Unknown name is ignored (keeps the configured default) rather
    // than refusing every block as unpriced.
    pub fn set_model(&mut self, model: &str) {
        if secondwind_ledger::rates_for(model).is_some() {
            self.gate.set_model(model);
        }
    }

    pub fn with_model(mut self, model: &str) -> Self {
        self.set_model(model);
        self
    }

    // Opt-in lossy-but-recoverable prose summary inline. Off by default (the lossless path runs otherwise).
    pub fn with_prose_mode(mut self, on: bool) -> Self {
        self.prose_mode = on;
        self
    }

    pub fn with_prose_shrinker(mut self, shrinker: Arc<dyn prose::ProseShrinker>) -> Self {
        self.prose_shrinker = Some(shrinker);
        self.prose_mode = true;
        self
    }

    // Switch the pipeline from the default byte proxy to a real token counter, so codec selection
    // and every gate decision optimize token cost.
    pub fn with_counter(mut self, counter: Arc<dyn TokenCounter>) -> Self {
        // Index 0 is always the built-in columnar; rebuild only it so a with_transform addition survives.
        self.transforms[0] = Box::new(Columnar::with_counter(counter.clone()));
        self.counter = counter;
        self
    }

    // Custom transform, tried after the built-ins. Passes the same lossless + net-cost gate, so a
    // lossy or unprofitable one is refused, never shipped.
    pub fn with_transform(mut self, transform: Box<dyn Transform>) -> Self {
        self.transforms.push(transform);
        self
    }

    // Host-supplied text codec, searched best-of-N alongside the built-ins. decode(encode(raw)) == raw
    // proven per-instance, so even a reckless codec is dropped on a wrong round-trip, never shipped.
    pub fn with_text_proposer(mut self, proposer: Box<dyn TextProposer>) -> Self {
        self.text_proposers.push(proposer);
        self
    }

    // Toggle the best-of-N proposer search (on by default). A cost/latency preference, not a safety
    // switch (proposals are proven lossless); off, punted blocks fall straight to offload/verbatim.
    pub fn set_proposers_enabled(&mut self, enabled: bool) {
        self.proposers_enabled = enabled;
    }

    // Back the offload store with any resolvable backend (default local disk; Store::persistent for a
    // durable TTL-bounded dir; a shared OffloadStore backend for multi-instance).
    pub fn with_store(mut self, store: impl OffloadStore + 'static) -> Self {
        self.store = Arc::new(store);
        self
    }

    // A fleet-shared store: built once, handed to every per-request optimizer as a cheap Arc clone.
    pub fn with_shared_store(mut self, store: Arc<dyn OffloadStore>) -> Self {
        self.store = store;
        self
    }

    fn priced(&self, before: &str, after: &str) -> Verdict {
        if self.counter.native() {
            self.gate.score_tokens(
                self.zone,
                self.counter.count(before),
                self.counter.count(after),
            )
        } else {
            self.gate.score(self.zone, before.len(), after.len())
        }
    }

    // Configured unit for a string: tokens when a tokenizer is set, else bytes. Public so a proxy
    // reports savings in the unit the gate priced.
    pub fn count(&self, text: &str) -> usize {
        self.counter.count(text)
    }

    // Net saving of before to after through the pipeline's gate, None if it does not clear.
    pub fn saving_usd(&self, before: &str, after: &str) -> Option<f64> {
        match self.priced(before, after) {
            Verdict::Save { usd } => Some(usd),
            Verdict::Refuse(_) => None,
        }
    }

    // The unit the pipeline optimizes: tokens when a tokenizer is set, else bytes.
    fn units(&self, text: &str) -> usize {
        if self.counter.native() {
            self.counter.count(text)
        } else {
            text.len()
        }
    }

    // Query-aware: keep the rows a request is about inline, offload the rest recoverably.
    pub fn compress_block_with_query(&mut self, raw: &str, query: &str) -> Outcome {
        if let Some(outcome) = self.try_relevance_split(raw, query) {
            return outcome;
        }
        self.compress_block(raw)
    }

    // A byte-identical repeat carries no new atom: collapse it to a content-hashed marker instead of
    // a second full preview. None when too small or it does not price out (caller compresses on merits).
    pub fn offload_repeat(&mut self, raw: &str) -> Option<Outcome> {
        let offloaded = self.store.offload(raw).ok()?;
        if !self.store.covers(&offloaded.marker, raw) {
            return None;
        }
        let stub = format!(
            "[identical to an earlier tool result]\n{}",
            offloaded.marker
        );
        match self.priced(raw, &stub) {
            Verdict::Save { usd } => Some(Outcome::Offloaded {
                stub,
                marker: offloaded.marker,
                saved_usd: usd,
            }),
            Verdict::Refuse(_) => None,
        }
    }

    fn try_relevance_split(&mut self, raw: &str, query: &str) -> Option<Outcome> {
        let value = serde_json::from_str::<Value>(raw).ok()?;
        let items = value.as_array()?;
        if items.len() < RELEVANCE_MIN_ROWS {
            return None;
        }
        let rows: Vec<String> = items.iter().map(atom::canonicalize).collect();
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let keep = (items.len() / 4).clamp(1, RELEVANCE_MAX_INLINE);
        let kept = relevance::select(&refs, query, keep, self.embedder.as_ref());
        if kept.is_empty() || kept.len() == items.len() {
            return None;
        }

        let relevant = Value::Array(kept.iter().map(|&i| items[i].clone()).collect());
        let rest = Value::Array(
            (0..items.len())
                .filter(|i| !kept.contains(i))
                .map(|i| items[i].clone())
                .collect(),
        );
        let inline = Columnar::default()
            .try_encode(&relevant)
            .map(|encoded| encoded.wire)
            .unwrap_or_else(|| atom::canonicalize(&relevant));
        if !detectorgate::detector_findings(&atom::canonicalize(&relevant), &inline).is_empty() {
            return None;
        }

        let rest_json = atom::canonicalize(&rest);
        let offloaded = self.store.offload(&rest_json).ok()?;
        if !self.store.covers(&offloaded.marker, &rest_json) {
            return None;
        }
        let rest_count = items.len() - kept.len();
        let stub = format!(
            "{inline}\n[{rest_count} rows less relevant to the query, call resolve for them]\n{}",
            offloaded.marker
        );

        match self.priced(raw, &stub) {
            Verdict::Save { usd } => Some(Outcome::Offloaded {
                stub,
                marker: offloaded.marker,
                saved_usd: usd,
            }),
            Verdict::Refuse(_) => None,
        }
    }

    pub fn compress_block(&mut self, raw: &str) -> Outcome {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            if self.prose_mode
                && let Some(outcome) = self
                    .try_prose_shrink(raw)
                    .or_else(|| self.try_prose_split(raw))
            {
                return outcome;
            }
            return self.compress_text(raw);
        };

        for transform in &self.transforms {
            let Some(encoded) = transform.try_encode(&value) else {
                continue;
            };
            let certificate = match admit(&value, &encoded, |v| transform.try_encode(v)) {
                Ok(certificate) => certificate,
                Err(refusal) => {
                    return Outcome::KeptVerbatim {
                        reason: KeptReason::Refused(transform.id(), refusal),
                    };
                }
            };
            // Price the counterfactual against raw (what the model would be billed), not canonical,
            // so the figure matches the raw-to-wire reduction the dashboard shows.
            let usd = match self.priced(raw, &encoded.wire) {
                Verdict::Save { usd } => usd,
                Verdict::Refuse(reason) => {
                    return Outcome::KeptVerbatim {
                        reason: KeptReason::NoNetSaving(reason),
                    };
                }
            };

            if !detectorgate::detector_findings(&atom::canonicalize(&value), &encoded.wire)
                .is_empty()
            {
                return Outcome::KeptVerbatim {
                    reason: KeptReason::DetectorFired,
                };
            }

            return Outcome::Compressed {
                wire: encoded.wire,
                transform: transform.id(),
                certificate,
                saved_usd: usd,
            };
        }

        self.try_offload(raw)
    }

    fn try_search(&mut self, raw: &str) -> Outcome {
        let Some(factored) = search::try_factor(raw) else {
            return self.try_text_columnar(raw);
        };
        if let Some(preview) = offload::preview_if_offloaded(raw) {
            let marker = format!("<<swload:{}>>", "0".repeat(16));
            let stub = format!("{preview}\n{marker}");
            let offload_expected = self.units(&stub) as f64 + REOPEN_PRIOR * self.units(raw) as f64;
            if offload_expected < self.units(&factored.wire) as f64 {
                return self.try_offload(raw);
            }
        }
        let Verdict::Save { usd } = self.priced(raw, &factored.wire) else {
            return self.try_text_columnar(raw);
        };
        if !detectorgate::detector_findings(raw, &factored.wire).is_empty() {
            return self.try_text_columnar(raw);
        }
        let wire_bytes = factored.wire.len();
        Outcome::Compressed {
            wire: factored.wire,
            transform: "search",
            certificate: Certificate {
                clmh_before: clmh::Clmh::default(),
                clmh_after: clmh::Clmh::default(),
                canonical_bytes: raw.len(),
                wire_bytes,
            },
            saved_usd: usd,
        }
    }

    // Lossless columnar reformat of aligned tabular output (ls, ps, df, docker/kubectl), read inline
    // with no round-trip. Defers to offload when a recoverable offload beats keeping the columns inline.
    fn try_text_columnar(&mut self, raw: &str) -> Outcome {
        let encoded = {
            let cost = |s: &str| self.units(s);
            text_columnar::try_encode(raw, &cost)
        };
        let Some(encoded) = encoded else {
            return self.try_log(raw);
        };
        if self.offload_allowed
            && let Some(preview) = offload::preview_if_offloaded(raw)
        {
            let marker = format!("<<swload:{}>>", "0".repeat(16));
            let stub = format!("{preview}\n{marker}");
            let offload_expected = self.units(&stub) as f64 + REOPEN_PRIOR * self.units(raw) as f64;
            if offload_expected < self.units(&encoded.wire) as f64 {
                return self.try_offload(raw);
            }
        }
        let Verdict::Save { usd } = self.priced(raw, &encoded.wire) else {
            return self.try_log(raw);
        };
        if !detectorgate::detector_findings(raw, &encoded.wire).is_empty() {
            return self.try_log(raw);
        }
        let wire_bytes = encoded.wire.len();
        Outcome::Compressed {
            wire: encoded.wire,
            transform: "columns",
            certificate: Certificate {
                clmh_before: clmh::Clmh::default(),
                clmh_after: clmh::Clmh::default(),
                canonical_bytes: raw.len(),
                wire_bytes,
            },
            saved_usd: usd,
        }
    }

    fn try_log(&mut self, raw: &str) -> Outcome {
        let Some(templated) = log::try_template(raw) else {
            return self.try_proposers(raw);
        };
        let Verdict::Save { usd } = self.priced(raw, &templated.wire) else {
            return self.try_proposers(raw);
        };
        if !detectorgate::detector_findings(raw, &templated.wire).is_empty() {
            return self.try_proposers(raw);
        }
        let wire_bytes = templated.wire.len();
        Outcome::Compressed {
            wire: templated.wire,
            transform: "log",
            certificate: Certificate {
                clmh_before: clmh::Clmh::default(),
                clmh_after: clmh::Clmh::default(),
                canonical_bytes: raw.len(),
                wire_bytes,
            },
            saved_usd: usd,
        }
    }

    // Text branch: run the built-in cascade, then let proposers compete with its inline wire, making
    // the search best-of-N across ALL codecs. The offload comparison the built-ins ran still holds.
    fn compress_text(&mut self, raw: &str) -> Outcome {
        let cascade = self.try_search(raw);
        if let Outcome::Compressed { wire: ref cw, .. } = cascade {
            let cascade_units = self.units(cw);
            if let Some((wire, id, usd, units)) = self.best_proposer(raw)
                && units < cascade_units
            {
                // The built-in already beat offload to win the cascade, so a smaller proposer beats it
                // too: ship it without re-checking offload.
                let wire_bytes = wire.len();
                return Outcome::Compressed {
                    wire,
                    transform: id,
                    certificate: Certificate {
                        clmh_before: clmh::Clmh::default(),
                        clmh_after: clmh::Clmh::default(),
                        canonical_bytes: raw.len(),
                        wire_bytes,
                    },
                    saved_usd: usd,
                };
            }
        }
        cascade
    }

    // Best-of-N over the text proposers, keeping only candidates whose own decode reproduces the block
    // (reckless-safe per-instance proof) and clear price + detector. Deterministic tie-break so the wire is stable.
    fn best_proposer(&mut self, raw: &str) -> Option<(String, &'static str, f64, usize)> {
        if !self.proposers_enabled {
            return None;
        }
        let mut best: Option<(String, &'static str, f64, usize)> = None;
        for i in 0..self.text_proposers.len() {
            let Some(wire) = self.text_proposers[i].encode(raw) else {
                continue;
            };
            if self.text_proposers[i].decode(&wire).as_deref() != Some(raw) {
                continue; // reckless proposal that does not round-trip: dropped, never shipped
            }
            let Verdict::Save { usd } = self.priced(raw, &wire) else {
                continue;
            };
            if !detectorgate::detector_findings(raw, &wire).is_empty() {
                continue;
            }
            let units = self.units(&wire);
            let id = self.text_proposers[i].id();
            let better = match &best {
                Some((bw, bid, _, bunits)) => {
                    units < *bunits
                        || (units == *bunits && (id, wire.as_str()) < (*bid, bw.as_str()))
                }
                None => true,
            };
            if better {
                best = Some((wire, id, usd, units));
            }
        }
        best
    }

    // Proposer tier: reached when structured codecs punt. Ships the best proposer unless a
    // recoverable offload is cheaper.
    fn try_proposers(&mut self, raw: &str) -> Outcome {
        if let Some((wire, id, usd, units)) = self.best_proposer(raw) {
            if self.offload_allowed
                && let Some(preview) = offload::preview_if_offloaded(raw)
            {
                let marker = format!("<<swload:{}>>", "0".repeat(16));
                let stub = format!("{preview}\n{marker}");
                let offload_expected =
                    self.units(&stub) as f64 + REOPEN_PRIOR * self.units(raw) as f64;
                if offload_expected < units as f64 {
                    return self.try_offload(raw);
                }
            }
            let wire_bytes = wire.len();
            return Outcome::Compressed {
                wire,
                transform: id,
                certificate: Certificate {
                    clmh_before: clmh::Clmh::default(),
                    clmh_after: clmh::Clmh::default(),
                    canonical_bytes: raw.len(),
                    wire_bytes,
                },
                saved_usd: usd,
            };
        }
        self.try_offload(raw)
    }

    // Opt-in lossy-but-recoverable prose: coherent summary inline, full original offloaded behind a
    // marker. Surfaced only when a resolver is present, so without recovery the block stays whole.
    fn try_prose_split(&mut self, raw: &str) -> Option<Outcome> {
        if raw.len() < PROSE_SUMMARY_MIN_BYTES {
            return None;
        }
        let budget = (raw.len() as f64 * PROSE_SUMMARY_FRACTION) as usize;
        let summary = prose::summary(raw, budget, &prose::CoverageScorer)?;
        self.offload_behind(raw, summary)
    }

    fn try_prose_shrink(&mut self, raw: &str) -> Option<Outcome> {
        if raw.len() < PROSE_SUMMARY_MIN_BYTES {
            return None;
        }
        let spans = self.prose_shrinker.as_ref()?.keep(raw)?;
        let shrunk = prose::shrink(raw, &spans)?;
        self.offload_behind(raw, shrunk)
    }

    fn offload_behind(&mut self, raw: &str, inline: String) -> Option<Outcome> {
        let offloaded = self.store.offload(raw).ok()?;
        if !self.store.covers(&offloaded.marker, raw) {
            return None;
        }
        let stub = format!("{inline}\n{}", offloaded.marker);
        match self.priced(raw, &stub) {
            Verdict::Save { usd } => Some(Outcome::Offloaded {
                stub,
                marker: offloaded.marker,
                saved_usd: usd,
            }),
            Verdict::Refuse(_) => None,
        }
    }

    fn try_offload(&mut self, raw: &str) -> Outcome {
        if !self.offload_allowed {
            return Outcome::KeptVerbatim {
                reason: KeptReason::NotApplicable,
            };
        }
        let offloaded = match self.store.offload(raw) {
            Ok(offloaded) => offloaded,
            Err(_) => {
                return Outcome::KeptVerbatim {
                    reason: KeptReason::NotApplicable,
                };
            }
        };
        if !self.store.covers(&offloaded.marker, raw) {
            return Outcome::KeptVerbatim {
                reason: KeptReason::DetectorFired,
            };
        }
        match self.priced(raw, &offloaded.stub) {
            Verdict::Save { usd } => Outcome::Offloaded {
                stub: offloaded.stub,
                marker: offloaded.marker,
                saved_usd: usd,
            },
            Verdict::Refuse(reason) => Outcome::KeptVerbatim {
                reason: KeptReason::NoNetSaving(reason),
            },
        }
    }

    pub fn resolve(&self, marker: &str) -> Option<String> {
        self.store.resolve(marker)
    }
}

// Test-only: corrupts a value so tests can prove the gate refuses corruption.
pub struct CorruptingTransform;

impl Transform for CorruptingTransform {
    fn id(&self) -> &'static str {
        "corrupting-test-only"
    }

    fn try_encode(&self, value: &Value) -> Option<Encoded> {
        let mut items = value.as_array()?.clone();
        let first = items.first_mut()?.as_object_mut()?;
        let key = first.keys().next()?.clone();
        first.insert(key, Value::Number(999_999.into()));
        Some(Encoded {
            wire: serde_json::to_string(&items).ok()?,
            decoded: Value::Array(items),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A host-supplied codec that only fires on a long single-character run. Lossless by construction.
    struct Rle;
    impl TextProposer for Rle {
        fn id(&self) -> &'static str {
            "rle"
        }
        fn encode(&self, raw: &str) -> Option<String> {
            let c = raw.chars().next()?;
            (raw.len() > 8 && raw.chars().all(|x| x == c))
                .then(|| format!("RLE\u{1}{c}\u{1}{}", raw.chars().count()))
        }
        fn decode(&self, wire: &str) -> Option<String> {
            let (c, n) = wire.strip_prefix("RLE\u{1}")?.split_once('\u{1}')?;
            Some(c.chars().next()?.to_string().repeat(n.parse().ok()?))
        }
    }

    // A reckless codec whose decode does NOT reproduce the input: the gate must drop it.
    struct Corrupt;
    impl TextProposer for Corrupt {
        fn id(&self) -> &'static str {
            "corrupt"
        }
        fn encode(&self, raw: &str) -> Option<String> {
            (raw.len() > 8).then(|| format!("C{}", &raw[..raw.len() / 2]))
        }
        fn decode(&self, wire: &str) -> Option<String> {
            wire.strip_prefix('C').map(str::to_string)
        }
    }

    #[test]
    fn a_host_proposer_wins_the_search_where_the_built_ins_punt() {
        let raw = "x".repeat(500);
        let mut opt = Optimizer::default().with_text_proposer(Box::new(Rle));
        opt.set_offload_allowed(false);
        match opt.compress_block(&raw) {
            Outcome::Compressed {
                wire, transform, ..
            } => {
                assert_eq!(
                    transform, "rle",
                    "a host codec competes in the best-of-N search"
                );
                assert_eq!(
                    Rle.decode(&wire).as_deref(),
                    Some(raw.as_str()),
                    "and its wire round-trips"
                );
            }
            _ => panic!("the host proposer should compress a long run the built-ins skip"),
        }
    }

    #[test]
    fn a_reckless_proposal_that_does_not_round_trip_is_dropped() {
        let raw = "x".repeat(500);
        let mut opt = Optimizer::default().with_text_proposer(Box::new(Corrupt));
        opt.set_offload_allowed(false);
        // Corrupt loses half the block, so decode != raw: proven wrong per-instance, never shipped.
        // Nothing else fires on a bare run, so the block stays verbatim.
        assert!(matches!(
            opt.compress_block(&raw),
            Outcome::KeptVerbatim { .. }
        ));
    }

    #[test]
    fn the_best_of_n_search_never_enlarges_the_wire_and_toggles_off() {
        let raw: String = (0..70)
            .map(|i| format!("root {} 0.1 0.4 R /usr/local/bin/secondwind-service --tenant acme --config /etc/secondwind/prod.toml step-{i}", 1000 + i))
            .collect::<Vec<_>>()
            .join("\n");
        let wire_len = |proposers: bool| {
            let mut opt = Optimizer::default();
            opt.set_offload_allowed(false);
            opt.set_proposers_enabled(proposers);
            match opt.compress_block(&raw) {
                Outcome::Compressed { wire, .. } => {
                    assert!(
                        certificate::verify(&wire, &certificate::certify(&raw)),
                        "every shipped wire verifies lossless"
                    );
                    wire.len()
                }
                _ => raw.len(),
            }
        };
        assert!(
            wire_len(true) <= wire_len(false),
            "best-of-N with proposers is never worse than without"
        );
    }

    fn prose_block() -> String {
        [
            "The authentication service validates every request against the session store before any handler executes.",
            "Tokens expire after 3600 seconds and are rotated on each privileged action by a dedicated background worker.",
            "When a token is missing the gateway returns a 401 response and logs the client address for the audit trail.",
            "Rotation runs in a separate process so a slow validator never blocks the main request handling path.",
            "The session store is a Redis cluster with three replicas so a single node failure does not drop sessions.",
            "Metrics are exported every ten seconds and alert whenever the rejection rate climbs above two percent.",
            "The gateway strips forwarded headers so a client cannot spoof its source address into the audit log.",
            "Each session record carries the user identifier, the issued timestamp, and the scope list granted at login.",
            "Revocation is immediate because the validator checks a shared deny list before accepting any cached token.",
            "Load shedding kicks in above ten thousand requests per second and returns a 503 with a retry hint header.",
            "The deployment runs behind three availability zones and fails over automatically when a zone goes dark.",
            "Configuration reloads without a restart so operators can rotate signing keys during a live incident.",
        ]
        .join(" ")
    }

    #[test]
    fn tabular_output_routes_to_the_columns_transform_and_reconstructs() {
        // Sized above the columnar floor but below the offload floor, so it takes the inline columns path.
        let mut raw = String::new();
        for i in 0..10 {
            raw.push_str(&format!(
                "-rw-r--r-- 1 root wheel {:>5} f-{i}\n",
                100 + i * 37
            ));
        }
        let raw = raw.trim_end();
        assert!(
            raw.len() >= 256 && raw.len() < 512,
            "sized into the columns band: {}",
            raw.len()
        );

        let mut opt = Optimizer::default();
        let Outcome::Compressed {
            wire, transform, ..
        } = opt.compress_block(raw)
        else {
            panic!("aligned tabular output should take the columns path");
        };
        assert_eq!(transform, "columns");
        assert_eq!(
            text_columnar::decode(&wire).as_deref(),
            Some(raw),
            "byte-exact reconstruction"
        );
    }

    #[test]
    fn offload_gate_is_a_noop_without_a_resolver() {
        // A block only offload can handle (single-token lines: not column/search/log shaped). Offloads
        // when allowed; without a resolver stays verbatim rather than emit a marker nothing could resolve.
        let mut seed = 0xabcd_1234u32;
        let mut raw = String::new();
        for _ in 0..30 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let a = seed;
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            raw.push_str(&format!("{a:08x}{seed:08x}{a:08x}{seed:08x}\n"));
        }
        let raw = raw.trim_end();
        assert!(raw.len() > 512, "large enough to offload: {}", raw.len());

        assert!(
            matches!(
                Optimizer::default().compress_block(raw),
                Outcome::Offloaded { .. }
            ),
            "with offload allowed this block offloads"
        );

        let mut no_resolver = Optimizer::default();
        no_resolver.set_offload_allowed(false);
        assert!(
            matches!(
                no_resolver.compress_block(raw),
                Outcome::KeptVerbatim { .. }
            ),
            "without a resolver the offload path is a no-op, block kept whole"
        );
    }

    #[test]
    fn without_a_resolver_tabular_fires_columns_inline() {
        // The Codex-today path: a large aligned block with no resolver present compresses inline
        // via columns (a same-turn lossless win) instead of being kept whole.
        let mut raw = String::new();
        for i in 0..80 {
            raw.push_str(&format!(
                "-rw-r--r-- 1 root wheel {:>7} file-{i}.dat\n",
                1000 + i * 137
            ));
        }
        let raw = raw.trim_end();
        assert!(raw.len() > 512, "large enough to offload: {}", raw.len());

        let mut no_resolver = Optimizer::default();
        no_resolver.set_offload_allowed(false);
        let Outcome::Compressed {
            wire, transform, ..
        } = no_resolver.compress_block(raw)
        else {
            panic!("without a resolver the block should compress inline via columns");
        };
        assert_eq!(transform, "columns");
        assert_eq!(
            text_columnar::decode(&wire).as_deref(),
            Some(raw),
            "byte-exact"
        );
    }

    #[test]
    fn prose_mode_summarizes_inline_and_keeps_the_full_text_recoverable() {
        let raw = prose_block();
        assert!(raw.len() >= PROSE_SUMMARY_MIN_BYTES);

        let mut opt = Optimizer::default().with_prose_mode(true);
        let Outcome::Offloaded { stub, marker, .. } = opt.compress_block(&raw) else {
            panic!("prose mode should summarize a long prose block");
        };
        assert!(stub.contains("prose summary"));
        assert!(stub.contains("dropped"));
        assert!(stub.len() < raw.len());
        assert_eq!(opt.resolve(&marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn without_prose_mode_prose_is_left_to_the_lossless_path() {
        let raw = prose_block();
        let mut opt = Optimizer::default();
        if let Outcome::Offloaded { stub, .. } = opt.compress_block(&raw) {
            assert!(
                !stub.contains("prose summary"),
                "no lossy summary unless the mode is opted in"
            );
        }
    }

    struct HalfShrinker;
    impl prose::ProseShrinker for HalfShrinker {
        fn keep(&self, text: &str) -> Option<Vec<prose::Span>> {
            let mut mid = text.len() / 2;
            while !text.is_char_boundary(mid) {
                mid += 1;
            }
            Some(vec![prose::Span { start: 0, end: mid }])
        }
    }

    #[test]
    fn prose_shrinker_keeps_spans_inline_and_recovers_the_original() {
        let raw = prose_block();
        let mut opt = Optimizer::default().with_prose_shrinker(Arc::new(HalfShrinker));
        let Outcome::Offloaded { stub, marker, .. } = opt.compress_block(&raw) else {
            panic!("the shrinker should shrink a long prose block");
        };
        assert!(stub.contains("prose shrunk"));
        assert!(stub.len() < raw.len());
        assert_eq!(opt.resolve(&marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn relevance_split_keeps_the_query_rows_inline_and_recovers_the_rest() {
        let mut rows: Vec<String> = (0..40)
            .map(|i| format!(r#"{{"id":{i},"note":"shipping record number {i} for delivery"}}"#))
            .collect();
        rows.push(
            r#"{"id":900,"note":"authentication token rotated for the admin account"}"#.into(),
        );
        rows.push(
            r#"{"id":901,"note":"authentication failure logged for a stale session"}"#.into(),
        );
        let raw = format!("[{}]", rows.join(","));

        let mut optimizer = Optimizer::default();
        let Outcome::Offloaded { stub, marker, .. } =
            optimizer.compress_block_with_query(&raw, "authentication token")
        else {
            panic!("expected a relevance-split offload");
        };
        assert!(stub.contains("authentication token rotated"));
        assert!(stub.contains("call resolve"));
        let recovered = optimizer.resolve(&marker).expect("rest recovers");
        assert!(recovered.contains("shipping record number 0"));
    }
}
