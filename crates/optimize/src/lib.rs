#![forbid(unsafe_code)]

use serde_json::Value;

pub mod admit;
pub mod atom;
pub mod cachecost;
pub mod certificate;
pub mod clmh;
mod column;
pub mod columnar;
pub mod counterfactual;
pub mod dedup;
pub mod detectorgate;
pub mod dict;
pub mod distilled;
pub mod doc;
pub mod frontier;
pub mod frontlines;
pub mod grouped;
pub mod kv;
pub mod lines;
pub mod lock;
pub mod log;
pub mod nested;
pub mod netcost;
pub mod norm;
pub mod offload;
pub mod outline;
pub mod prefix;
pub mod prose;
pub mod proxy;
pub mod reconcile;
pub mod recordcol;
pub mod relevance;
pub mod replay;
pub mod resolve;
pub mod richness;
pub mod search;
pub mod select;
pub mod shape;
pub mod shred;
pub mod text_columnar;
pub mod tokens;
pub mod transform;
pub mod tree;
#[cfg(feature = "treesitter")]
pub mod treesit;

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
use doc::Doc;
use nested::Nested;
use netcost::{NetCostGate, Verdict, Zone};
use norm::Normalize;
use offload::{OffloadStore, Store};
use tokens::{ByteCounter, TokenCounter};
use transform::{Encoded, TextProposer, Transform};

type StructuredCandidate = (String, &'static str, Certificate, f64, usize);
type StructuredSearch = (Option<StructuredCandidate>, Option<KeptReason>);

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
    // Duplicate JSON keys: ambiguous, so kept verbatim rather than compressed.
    DuplicateKeys,
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
            KeptReason::DuplicateKeys => "duplicate_keys",
        }
    }
}

// Host control over recoverable eviction. Auto is the cost model: offload only when a content-covering
// preview makes it genuinely cheaper. Off never evicts (no resolver, or the block must stay in-window).
// Always evicts every offloadable block regardless of cost, for agents that will resolve on demand.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OffloadMode {
    Off,
    #[default]
    Auto,
    Always,
}

// Expected fraction of an offloaded block the agent pulls back via resolve; inline wins when its
// factored size beats stub + REOPEN_PRIOR * body. Measured 0.45 over 1055 real traces (2229 offloads).
const REOPEN_PRIOR: f64 = 0.45;

const RELEVANCE_MIN_ROWS: usize = 8;
const RELEVANCE_MAX_INLINE: usize = 24;

const PROSE_SUMMARY_MIN_BYTES: usize = 1000;
const PROSE_SUMMARY_FRACTION: f64 = 0.5;
const BUILTIN_TRANSFORM_COUNT: usize = 4;

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
    offload_mode: OffloadMode,
}

impl Default for Optimizer {
    fn default() -> Self {
        Self::new(NetCostGate::new("claude-sonnet-4-5", 1), Zone::Suffix)
    }
}

impl Optimizer {
    pub fn new(gate: NetCostGate, zone: Zone) -> Self {
        let counter: Arc<dyn TokenCounter> = Arc::new(ByteCounter);
        Self {
            transforms: Self::built_in_transforms(counter.clone()),
            // Built-in text codecs are unreadable inline; only host-registered proposers ship here,
            // and a host owns its codec's readability. See compress_text.
            text_proposers: Vec::new(),
            proposers_enabled: true,
            gate,
            zone,
            store: Arc::new(Store::default()),
            counter,
            embedder: Arc::new(distilled::DistilledEmbedder),
            prose_mode: false,
            prose_shrinker: None,
            offload_mode: OffloadMode::Auto,
        }
    }

    // Keep all built-in structured codecs on the same token counter. Custom transforms remain
    // appended after this fixed prefix and retain their own construction/configuration.
    fn built_in_transforms(counter: Arc<dyn TokenCounter>) -> Vec<Box<dyn Transform>> {
        vec![
            Box::new(Columnar::with_counter(counter.clone())),
            Box::new(Normalize::with_counter(counter.clone())),
            Box::new(Nested::with_counter(counter.clone())),
            Box::new(Doc::with_counter(counter)),
        ]
    }

    // Pick how recoverable eviction competes with inline: Off, Auto (cost model), or Always.
    pub fn set_offload_mode(&mut self, mode: OffloadMode) {
        self.offload_mode = mode;
    }

    pub fn offload_mode(&self) -> OffloadMode {
        self.offload_mode
    }

    // Off means no recoverable eviction on any path, so every direct-offload strategy (repeat dedup,
    // relevance split, prose split) checks this before touching the store.
    fn eviction_off(&self) -> bool {
        matches!(self.offload_mode, OffloadMode::Off)
    }

    // Compat shim: false means no resolver is present, so never offload (Off); true restores the
    // Auto cost model. A host wanting forced eviction calls set_offload_mode(Always) directly.
    pub fn set_offload_allowed(&mut self, allowed: bool) {
        self.offload_mode = if allowed {
            OffloadMode::Auto
        } else {
            OffloadMode::Off
        };
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
        // Rebuild the fixed built-in prefix without disturbing host transforms appended after it.
        for (slot, transform) in Self::built_in_transforms(counter.clone())
            .into_iter()
            .enumerate()
        {
            self.transforms[slot] = transform;
        }
        debug_assert!(self.transforms.len() >= BUILTIN_TRANSFORM_COUNT);
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
        if self.eviction_off() {
            return None;
        }
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
        if self.eviction_off() {
            return None;
        }
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

        // Duplicate JSON keys are ambiguous (last-wins drops a value); keep the block verbatim so
        // every value reaches the model, never certified as a compressed-lossless rewrite.
        if atom::has_duplicate_keys(raw) {
            return Outcome::KeptVerbatim {
                reason: KeptReason::DuplicateKeys,
            };
        }

        let (best, refusal) = self.best_structured(raw, &value);
        if let Some((wire, id, certificate, usd, _)) = best {
            return self.ship_inline_or_offload(raw, wire, id, certificate, usd);
        }

        let outcome = self.try_offload(raw);
        if matches!(
            &outcome,
            Outcome::KeptVerbatim {
                reason: KeptReason::NotApplicable
            }
        ) && let Some(reason) = refusal
        {
            Outcome::KeptVerbatim { reason }
        } else {
            outcome
        }
    }

    // Best-of-N for structured codecs: prove, price, and detector-check every applicable wire,
    // then use the actual configured token counter to pick the cheapest. Stable transform-id and
    // byte-length ties keep the selected wire deterministic for prompt-cache reuse.
    fn best_structured(&self, raw: &str, value: &Value) -> StructuredSearch {
        let canonical = atom::canonicalize(value);
        let mut best: Option<StructuredCandidate> = None;
        let mut refusal = None;
        for transform in &self.transforms {
            let Some(encoded) = transform.try_encode(value) else {
                continue;
            };
            let certificate = match admit(
                value,
                &encoded,
                |v| transform.try_encode(v),
                !transform.trusted(),
            ) {
                Ok(certificate) => certificate,
                Err(reason) => {
                    refusal.get_or_insert(KeptReason::Refused(transform.id(), reason));
                    continue;
                }
            };
            // Price the counterfactual against raw (what the model would be billed), not canonical,
            // so the figure matches the raw-to-wire reduction the dashboard shows.
            // A codec that encodes but does not shrink (map-heavy JSON: npm/PyPI registry responses,
            // where the bulk is object maps, not arrays) must not short-circuit to verbatim: skip it
            // so the block still reaches offload, which can evict a large incompressible body.
            let Verdict::Save { usd } = self.priced(raw, &encoded.wire) else {
                continue;
            };

            if !detectorgate::detector_findings(&canonical, &encoded.wire).is_empty() {
                refusal.get_or_insert(KeptReason::DetectorFired);
                continue;
            }

            let units = self.counter.count(&encoded.wire);
            let id = transform.id();
            let wire_bytes = encoded.wire.len();
            let better = match &best {
                Some((best_wire, best_id, _, _, best_units)) => {
                    (units, id, wire_bytes) < (*best_units, *best_id, best_wire.len())
                }
                None => true,
            };
            if better {
                best = Some((encoded.wire, id, certificate, usd, units));
            }
        }
        (best, refusal)
    }

    // Readable inline text codecs ship first: grouped (grep by file, listings by directory) and tree
    // (box-drawing dep trees as indentation). Both read at face value. Everything else has no readable
    // inline codec: a host proposer may ship (it owns its readability), else evict/verbatim.
    fn compress_text(&mut self, raw: &str) -> Outcome {
        for (id, wire) in [
            ("grouped", grouped::encode(raw)),
            ("tree", tree::encode(raw)),
            ("lock", lock::encode(raw)),
        ] {
            if let Some(wire) = wire
                && let Some(outcome) = self.try_text_codec(raw, id, wire)
            {
                return outcome;
            }
        }
        self.try_proposers(raw)
    }

    fn try_text_codec(&mut self, raw: &str, id: &'static str, wire: String) -> Option<Outcome> {
        let Verdict::Save { usd } = self.priced(raw, &wire) else {
            return None;
        };
        if !detectorgate::detector_findings(raw, &wire).is_empty() {
            return None;
        }
        let certificate = Certificate {
            clmh_before: clmh::Clmh::default(),
            clmh_after: clmh::Clmh::default(),
            canonical_bytes: raw.len(),
            wire_bytes: wire.len(),
        };
        Some(self.ship_inline_or_offload(raw, wire, id, certificate, usd))
    }

    // Whether recoverable eviction should be taken for this block, per the host's mode. Auto defers to
    // the cost model; Always forces it; Off refuses. wire_units is the inline candidate the offload
    // must beat under Auto.
    fn wants_offload(&self, raw: &str, wire_units: usize) -> bool {
        match self.offload_mode {
            OffloadMode::Off => false,
            OffloadMode::Always => true,
            OffloadMode::Auto => self.auto_offload_wins(raw, wire_units),
        }
    }

    // Auto rule: offload only when its preview covers the content (so REOPEN_PRIOR holds) and its
    // expected cost (stub + REOPEN_PRIOR * raw) beats the inline wire. A shape-only or head preview
    // forces a resolve, so eviction saves nothing and the inline wire wins.
    fn auto_offload_wins(&self, raw: &str, wire_units: usize) -> bool {
        let Some(preview) = offload::covering_preview(raw) else {
            return false;
        };
        let marker = format!("<<swload:{}>>", "0".repeat(16));
        let stub = format!("{preview}\n{marker}");
        let offload_expected = self.units(&stub) as f64 + REOPEN_PRIOR * self.units(raw) as f64;
        offload_expected < wire_units as f64
    }

    // Ship the inline wire unless the mode elects eviction and the block can actually be offloaded;
    // a block below the offload floor keeps its inline wire rather than falling to verbatim.
    fn ship_inline_or_offload(
        &mut self,
        raw: &str,
        wire: String,
        id: &'static str,
        certificate: Certificate,
        usd: f64,
    ) -> Outcome {
        if self.wants_offload(raw, self.units(&wire))
            && let out @ Outcome::Offloaded { .. } = self.try_offload(raw)
        {
            return out;
        }
        Outcome::Compressed {
            wire,
            transform: id,
            certificate,
            saved_usd: usd,
        }
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
            if self.wants_offload(raw, units)
                && let out @ Outcome::Offloaded { .. } = self.try_offload(raw)
            {
                return out;
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
        if self.eviction_off() {
            return None;
        }
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
        if self.eviction_off() {
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

    // Resolve one slice of an offloaded block (see select::select); empty selector returns it whole.
    pub fn resolve_selected(&self, marker: &str, selector: &str) -> Option<String> {
        let block = self.store.resolve(marker)?;
        if selector.trim().is_empty() {
            return Some(block);
        }
        select::select(&block, selector.trim())
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

    // Run: SW_BENCH2=1 cargo test -p secondwind-optimize --release --features tiktoken \
    //        bench_stage_latency -- --nocapture --test-threads=1
    #[test]
    fn bench_stage_latency_and_percentiles() {
        if std::env::var_os("SW_BENCH2").is_none() {
            return;
        }
        use serde_json::{Value, json};
        use std::time::Instant;

        fn pct(sorted: &[f64], q: f64) -> f64 {
            if sorted.is_empty() {
                return 0.0;
            }
            let i = (q * (sorted.len() - 1) as f64).round() as usize;
            sorted[i.min(sorted.len() - 1)]
        }
        fn timeit<F: FnMut()>(m: usize, mut f: F) -> f64 {
            for _ in 0..(m / 10).max(1) {
                f();
            }
            let t = Instant::now();
            for _ in 0..m {
                f();
            }
            t.elapsed().as_secs_f64() * 1e6 / m as f64
        }
        // A realistic JSON-array tool output of `rows` records (k8s-pod-ish, high cardinality).
        fn block(rows: usize) -> String {
            let v: Vec<Value> = (0..rows)
                .map(|i| {
                    json!({
                        "name": format!("pod-{i}"),
                        "ns": format!("team-{}", i % 12),
                        "status": if i % 7 == 0 { "Pending" } else { "Running" },
                        "restarts": i % 5,
                        "cpu_m": (i * 37) % 2000,
                        "mem_mi": (i * 53) % 8192,
                        "ip": format!("10.{}.{}.{}", i % 256, (i / 256) % 256, i % 254 + 1),
                        "node": format!("node-{}", i % 20),
                        "age_s": i * 13
                    })
                })
                .collect();
            serde_json::to_string(&v).unwrap()
        }

        #[cfg(feature = "tiktoken")]
        let (mk, unit) = (
            || {
                Optimizer::default()
                    .with_counter(std::sync::Arc::new(crate::tokens::Tiktoken::cl100k()))
            },
            "tokens",
        );
        #[cfg(not(feature = "tiktoken"))]
        let (mk, unit) = (Optimizer::default, "bytes");

        eprintln!("\n=== full compress_block latency (release, warm; counter={unit}; us/call) ===");
        eprintln!(
            "{:>6} {:>9} {:>8}  {:>7} {:>7} {:>7} {:>7} {:>8} {:>8}  transform",
            "rows", "bytes", "~tok", "p50", "p90", "p95", "p99", "p99.9", "max"
        );
        for &rows in &[15usize, 200, 2000] {
            let (psize, warm, n) = match rows {
                r if r <= 20 => (256usize, 64usize, 8000usize),
                r if r <= 400 => (256, 32, 2500),
                _ => (48, 8, 400),
            };
            let pool: Vec<String> = (0..psize).map(|s| block(rows + s % 9)).collect();
            let mut opt = mk();
            for b in pool.iter().take(warm) {
                std::hint::black_box(opt.compress_block(b));
            }
            let mut ds = Vec::with_capacity(n);
            let mut tf = "verbatim";
            for k in 0..n {
                let b = &pool[k % pool.len()];
                let t = Instant::now();
                let out = opt.compress_block(b);
                ds.push(t.elapsed().as_secs_f64() * 1e6);
                tf = match &out {
                    Outcome::Compressed { transform, .. } => transform,
                    Outcome::Offloaded { .. } => "offload",
                    _ => "verbatim",
                };
            }
            let toks = opt.count(&pool[0]);
            ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!(
                "{:>6} {:>9} {:>8}  {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>8.1} {:>8.1}  {}",
                rows,
                pool[0].len(),
                toks,
                pct(&ds, 0.50),
                pct(&ds, 0.90),
                pct(&ds, 0.95),
                pct(&ds, 0.99),
                pct(&ds, 0.999),
                ds.last().copied().unwrap_or(0.0),
                tf
            );
        }

        eprintln!("\n=== stage breakdown on a 200-row block (mean us/call) ===");
        let raw = block(200);
        let value: Value = serde_json::from_str(&raw).unwrap();
        let opt = mk();
        let m = 1500;
        let parse = timeit(m, || {
            std::hint::black_box(serde_json::from_str::<Value>(&raw).ok());
        });
        let dup = timeit(m, || {
            std::hint::black_box(atom::has_duplicate_keys(&raw));
        });
        let enc = timeit(m, || {
            std::hint::black_box(opt.transforms[0].try_encode(&value));
        });
        let encoded = opt.transforms[0].try_encode(&value).unwrap();
        let trusted = opt.transforms[0].trusted();
        let adm = timeit(m, || {
            std::hint::black_box(
                admit(
                    &value,
                    &encoded,
                    |v| opt.transforms[0].try_encode(v),
                    !trusted,
                )
                .is_ok(),
            );
        });
        let price = timeit(m, || {
            std::hint::black_box(opt.priced(&raw, &encoded.wire));
        });
        let canon = atom::canonicalize(&value);
        let det = timeit(m, || {
            std::hint::black_box(detectorgate::detector_findings(&canon, &encoded.wire));
        });
        let mut opt2 = mk();
        let full = timeit(800, || {
            std::hint::black_box(opt2.compress_block(&raw));
        });
        eprintln!("  parse (serde_json)        {parse:>7.1}");
        eprintln!("  dup-key scan              {dup:>7.1}");
        eprintln!("  codec encode (columnar)   {enc:>7.1}");
        eprintln!("  admit (clmh + witness)    {adm:>7.1}   (re-encodes for idempotence)");
        eprintln!("  priced (tokenize)         {price:>7.1}");
        eprintln!("  detector suite            {det:>7.1}");
        eprintln!("  ------------------------------------");
        eprintln!(
            "  sum of stages             {:>7.1}",
            parse + dup + enc + adm + price + det
        );
        eprintln!("  full compress_block       {full:>7.1}");
    }

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
    fn small_tabular_text_stays_verbatim_and_readable() {
        // Tabular text has no model-readable inline codec; below the offload floor it stays verbatim
        // (readable) rather than reshaping into a wire the model cannot read.
        let mut raw = String::new();
        for i in 0..10 {
            raw.push_str(&format!(
                "{:>7} -rw-r--r-- 1 root wheel {:>5} f-{i}\n",
                13000 + i * 137,
                100 + i * 37
            ));
        }
        let raw = raw.trim_end();
        assert!(raw.len() < 512, "below the offload floor: {}", raw.len());

        assert!(
            matches!(
                Optimizer::default().compress_block(raw),
                Outcome::KeptVerbatim { .. }
            ),
            "small tabular text stays whole and readable"
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
    fn a_duplicate_key_block_is_kept_verbatim_not_compressed() {
        // Each row has a duplicate "amount" key; last-wins would drop 100, so keep the block whole.
        let rows: Vec<String> = (0..30)
            .map(|i| format!(r#"{{"id":{i},"amount":100,"amount":999,"name":"row_{i}"}}"#))
            .collect();
        let raw = format!("[{}]", rows.join(","));
        let Outcome::KeptVerbatim { reason } = Optimizer::default().compress_block(&raw) else {
            panic!("duplicate-key JSON must be kept verbatim, not compressed");
        };
        assert_eq!(reason.as_str(), "duplicate_keys");
    }

    #[test]
    fn without_a_resolver_tabular_text_stays_verbatim_readable() {
        // Tabular text has no model-readable inline codec, so with no resolver to offload behind it
        // stays whole and readable rather than reshaping into a wire the model cannot read.
        let mut raw = String::new();
        for i in 0..80 {
            raw.push_str(&format!(
                "-rw-r--r-- 1 root wheel {:>7} file-{i}.dat\n",
                1000 + i * 137
            ));
        }
        let raw = raw.trim_end();
        assert!(
            raw.len() > 512,
            "large enough to offload with a resolver: {}",
            raw.len()
        );

        let mut no_resolver = Optimizer::default();
        no_resolver.set_offload_allowed(false);
        assert!(
            matches!(
                no_resolver.compress_block(raw),
                Outcome::KeptVerbatim { .. }
            ),
            "without a resolver tabular text is kept whole and readable"
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
