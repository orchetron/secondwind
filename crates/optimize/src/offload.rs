use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::atom::{canonicalize, leaves};
use crate::clmh::Clmh;

// Hot in-process map plus (when persistent) a durable disk copy, so a marker still resolves after a
// restart and across surfaces. Past-TTL entries read as gone, bounding disk without dropping a live
// marker (touch-on-read keeps in-use markers alive).
pub struct Store {
    // Per-node hot cache; shared by &self. Durable copy is the disk, keyed by content hash.
    mem: RwLock<HashMap<String, String>>,
    dir: Option<PathBuf>,
    ttl: Duration,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            mem: RwLock::new(HashMap::new()),
            dir: None,
            ttl: DEFAULT_TTL,
        }
    }
}

// Resolvable byte store for offloaded blocks, shared by &self (Send + Sync) so one instance backs a
// whole fleet. Marker = blake3 of canonical content, so identical blocks write the same key with no
// coordination. Default is local disk; a networked deployment drops in Redis or object storage.
pub trait OffloadStore: Send + Sync {
    fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError>;
    fn resolve(&self, marker: &str) -> Option<String>;
    fn covers(&self, marker: &str, original: &str) -> bool;
    fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)>;
}

// Local-disk store: the default backend, replaceable by a custom store.
impl OffloadStore for Store {
    fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError> {
        Store::offload(self, raw)
    }
    fn resolve(&self, marker: &str) -> Option<String> {
        Store::resolve(self, marker)
    }
    fn covers(&self, marker: &str, original: &str) -> bool {
        Store::covers(self, marker, original)
    }
    fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)> {
        Store::prospective_stub_len(self, raw)
    }
}

#[derive(Debug)]
pub struct Offloaded {
    pub stub: String,
    pub marker: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffloadError {
    NotBigEnough,
    MarkerUnresolvable,
    // Backend momentarily unreachable: caller keeps the block inline and may retry, not unresolvable.
    Transient,
}

const MIN_OFFLOAD_BYTES: usize = 512;
const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

impl Store {
    // Disk-backed store rooted at dir. Construction does no directory scan; free `sweep` bounds it.
    pub fn persistent(dir: impl Into<PathBuf>, ttl: Duration) -> Self {
        let dir = dir.into();
        let _ = std::fs::create_dir_all(&dir);
        Self {
            mem: RwLock::new(HashMap::new()),
            dir: Some(dir),
            ttl,
        }
    }

    pub fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError> {
        let canonical = match serde_json::from_str::<Value>(raw) {
            Ok(value) => canonicalize(&value),
            Err(_) => raw.to_string(),
        };
        if canonical.len() < MIN_OFFLOAD_BYTES {
            return Err(OffloadError::NotBigEnough);
        }

        // Hash canonical (value-identical blocks share a marker); store raw so resolve returns the
        // original exactly, not canonicalized.
        let id = hash(&canonical)[..16].to_string();
        let marker = format!("<<swload:{id}>>");
        self.mem
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(marker.clone(), raw.to_string());
        if let Some(dir) = &self.dir {
            // Commit durably, then verify by reading the DURABLE path (not the hot copy), since a
            // separate process resolves from disk. On any IO failure, fail open: keep the block
            // verbatim rather than mint a marker with no backing bytes.
            let path = dir.join(format!("{id}.blob"));
            let committed = write_atomic(&path, raw.as_bytes()).is_ok()
                && std::fs::read_to_string(&path).ok().as_deref() == Some(raw);
            if !committed {
                self.mem
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&marker);
                let _ = std::fs::remove_file(&path);
                return Err(OffloadError::MarkerUnresolvable);
            }
        } else if self.resolve(&marker).as_deref() != Some(raw) {
            // In-memory-only store (no disk): prove the marker resolves before committing.
            self.mem
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&marker);
            return Err(OffloadError::MarkerUnresolvable);
        }

        let stub = format!("{}\n{marker}", preview(&canonical));
        Ok(Offloaded { stub, marker })
    }

    pub fn resolve(&self, marker: &str) -> Option<String> {
        if let Some(body) = self
            .mem
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(marker)
        {
            return Some(body.clone());
        }
        let dir = self.dir.as_ref()?;
        let id = marker_id(marker)?;
        let path = dir.join(format!("{id}.blob"));
        if self.expired(&path) {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        let body = std::fs::read_to_string(&path).ok()?;
        // The marker id is the content hash; a tampered blob must not resolve to the wrong bytes.
        let canonical = match serde_json::from_str::<Value>(&body) {
            Ok(value) => canonicalize(&value),
            Err(_) => body.clone(),
        };
        if &hash(&canonical)[..16] != id {
            return None;
        }
        // Touch on read so a marker still in active use never ages out from under a long session.
        touch(&path);
        Some(body)
    }

    fn expired(&self, path: &std::path::Path) -> bool {
        let Ok(modified) = path.metadata().and_then(|m| m.modified()) else {
            return true;
        };
        SystemTime::now()
            .duration_since(modified)
            .map(|age| age > self.ttl)
            .unwrap_or(false)
    }

    // Weighs offload against keeping a block inline without committing to the store.
    pub fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)> {
        let canonical = match serde_json::from_str::<Value>(raw) {
            Ok(value) => canonicalize(&value),
            Err(_) => raw.to_string(),
        };
        if canonical.len() < MIN_OFFLOAD_BYTES {
            return None;
        }
        let marker_len = format!("<<swload:{}>>", "0".repeat(16)).len();
        Some((preview(&canonical).len() + 1 + marker_len, canonical.len()))
    }

    // True iff every atom of the body is recoverable through the marker (stub drops nothing).
    pub fn covers(&self, marker: &str, original: &str) -> bool {
        let Some(body) = self.resolve(marker) else {
            return false;
        };
        let want = match serde_json::from_str::<Value>(original) {
            Ok(value) => Clmh::of(&leaves(&value)),
            Err(_) => return body == original,
        };
        let have = serde_json::from_str::<Value>(&body)
            .map(|value| Clmh::of(&leaves(&value)))
            .unwrap_or_default();
        want == have
    }
}

type PutCallback = Box<dyn Fn(&str, &str) -> bool + Send + Sync>;
type GetCallback = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

// Store whose durable backend is host-supplied: `put`/`get` over a content id; secondwind keeps
// marker, stub, coverage proof, and sizing. id = blake3 of canonical content, so two writers of the
// same block share a key with no coordination.
pub struct CallbackStore {
    put: PutCallback,
    get: GetCallback,
}

impl CallbackStore {
    pub fn new(
        put: impl Fn(&str, &str) -> bool + Send + Sync + 'static,
        get: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            put: Box::new(put),
            get: Box::new(get),
        }
    }
}

impl OffloadStore for CallbackStore {
    fn offload(&self, raw: &str) -> Result<Offloaded, OffloadError> {
        let canonical = canonical_of(raw);
        if canonical.len() < MIN_OFFLOAD_BYTES {
            return Err(OffloadError::NotBigEnough);
        }
        let id = hash(&canonical)[..16].to_string();
        let marker = format!("<<swload:{id}>>");
        // Prove the backend reads back before minting a marker; a failing backend keeps the block
        // verbatim rather than stranding an unresolvable marker.
        if !(self.put)(&id, raw) || (self.get)(&id).as_deref() != Some(raw) {
            return Err(OffloadError::MarkerUnresolvable);
        }
        Ok(Offloaded {
            stub: format!("{}\n{marker}", preview(&canonical)),
            marker,
        })
    }

    fn resolve(&self, marker: &str) -> Option<String> {
        (self.get)(marker_id(marker)?)
    }

    fn covers(&self, marker: &str, original: &str) -> bool {
        let Some(body) = self.resolve(marker) else {
            return false;
        };
        let want = match serde_json::from_str::<Value>(original) {
            Ok(value) => Clmh::of(&leaves(&value)),
            Err(_) => return body == original,
        };
        let have = serde_json::from_str::<Value>(&body)
            .map(|value| Clmh::of(&leaves(&value)))
            .unwrap_or_default();
        want == have
    }

    fn prospective_stub_len(&self, raw: &str) -> Option<(usize, usize)> {
        let canonical = canonical_of(raw);
        if canonical.len() < MIN_OFFLOAD_BYTES {
            return None;
        }
        let marker_len = format!("<<swload:{}>>", "0".repeat(16)).len();
        Some((preview(&canonical).len() + 1 + marker_len, canonical.len()))
    }
}

// The canonical form a block is hashed and sized by: JSON canonicalized, anything else verbatim.
fn canonical_of(raw: &str) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(value) => canonicalize(&value),
        Err(_) => raw.to_string(),
    }
}

// Deletes expired blobs. Called on a schedule, not per construction, so per-request cost stays flat.
pub fn sweep(dir: &Path, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "blob") {
            let expired = path
                .metadata()
                .and_then(|m| m.modified())
                .map(|modified| {
                    now.duration_since(modified)
                        .map(|age| age > ttl)
                        .unwrap_or(false)
                })
                .unwrap_or(true);
            if expired {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

// Temp file + fsync + atomic rename, so the separate resolver process never reads a torn blob.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("blob.{seq}.tmp"));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Err(err) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(())
}

// Best-effort mtime bump for touch-on-read.
fn touch(path: &Path) {
    let _ = filetime::set_file_mtime(path, filetime::FileTime::now());
}

fn marker_id(marker: &str) -> Option<&str> {
    marker.strip_prefix("<<swload:")?.strip_suffix(">>")
}

// The 16-hex offload marker id embedded in a stub (which also carries a preview), if well-formed.
pub fn embedded_marker_id(text: &str) -> Option<&str> {
    let start = text.find("<<swload:")? + "<<swload:".len();
    let end = text[start..].find(">>")? + start;
    let id = &text[start..end];
    (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

// Preview size scales with the block; this fraction is the one savings-vs-preview knob.
const PREVIEW_FRACTION: f64 = 0.05;
const MIN_PREVIEW_BYTES: usize = 160;

fn preview_budget(body_len: usize) -> usize {
    ((body_len as f64 * PREVIEW_FRACTION) as usize).max(MIN_PREVIEW_BYTES)
}

fn preview(canonical: &str) -> String {
    let budget = preview_budget(canonical.len());
    if let Some(summary) = diff_summary(canonical, budget) {
        return format!(
            "[{} bytes offloaded, diff summary, call resolve for the full patch]\n{summary}",
            canonical.len()
        );
    }
    if let Some(map) = crate::search::location_map(canonical, budget) {
        return format!(
            "[{} bytes offloaded, match map, call resolve for the matched lines]\n{map}",
            canonical.len()
        );
    }
    if let Some(outline) = crate::outline::outline(canonical, budget) {
        return format!(
            "[{} bytes offloaded, code outline, call resolve for the full body]\n{outline}",
            canonical.len()
        );
    }
    // Record arrays: show the short scalar content as a readable table so the agent answers common
    // lookups inline; long/boilerplate fields and overflow rows stay recoverable through resolve.
    if let Some(table) = content_table(canonical) {
        return table;
    }
    // Index shape, never values, so the model can't answer a value from the preview and must resolve.
    if let Some(index) = json_index(canonical, budget) {
        return format!(
            "[{} bytes offloaded, json index, values omitted, call resolve for any value]\n{index}",
            canonical.len()
        );
    }
    if let Some(digest) = crate::prose::digest(canonical, crate::prose::budget(canonical.len())) {
        return format!(
            "[{} bytes offloaded, prose digest, partial and not verbatim, call resolve for exact text]\n{digest}",
            canonical.len()
        );
    }
    let head: String = canonical.chars().take(80).collect();
    format!("[{} bytes offloaded, preview: {head} ...]", canonical.len())
}

// The first line is always kept, so a preview is never empty.
fn fit(lines: &[String], budget: usize, noun: &str) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 && out.len() + 1 + line.len() > budget {
            out.push_str(&format!("\n+{} more {noun}", lines.len() - i));
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

#[derive(Default)]
struct FileChange {
    path: String,
    added: u32,
    removed: u32,
    sections: Vec<String>,
}

impl FileChange {
    fn render(&self, budget: usize) -> String {
        let mut line = format!("{}  +{} -{}", self.path, self.added, self.removed);
        for (i, section) in self.sections.iter().enumerate() {
            let sep = if i == 0 { "  " } else { ", " };
            if line.len() + sep.len() + section.len() > budget {
                line.push_str(&format!(", +{} more", self.sections.len() - i));
                break;
            }
            line.push_str(sep);
            line.push_str(section);
        }
        line
    }
}

// Preview a diff by change surface: per file, net +/- and the functions its hunks touched.
fn diff_summary(text: &str, budget: usize) -> Option<String> {
    let mut files: Vec<FileChange> = Vec::new();
    let mut cur: Option<FileChange> = None;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(done) = cur.take() {
                files.push(done);
            }
            cur = Some(FileChange {
                path: rest.split_whitespace().next().unwrap_or(rest).to_string(),
                ..FileChange::default()
            });
        } else if let Some(file) = cur.as_mut() {
            if let Some(rest) = line.strip_prefix("@@")
                && let Some(section) = hunk_section(rest)
            {
                remember(&mut file.sections, section);
            } else if let Some(body) = line.strip_prefix('+')
                && !line.starts_with("+++")
            {
                file.added += 1;
                note_signature(&mut file.sections, body);
            } else if let Some(body) = line.strip_prefix('-')
                && !line.starts_with("---")
            {
                file.removed += 1;
                note_signature(&mut file.sections, body);
            }
        }
    }
    if let Some(done) = cur.take() {
        files.push(done);
    }

    if files.is_empty() {
        return None;
    }
    let per_file = budget / files.len().max(1);
    let lines: Vec<String> = files.iter().map(|f| f.render(per_file)).collect();
    Some(fit(&lines, budget, "files"))
}

// Text after the second `@@` is git's section heading: the enclosing declaration.
fn hunk_section(rest: &str) -> Option<String> {
    let close = rest.find("@@")?;
    let context = rest[close + 2..].trim();
    if context.is_empty() {
        return None;
    }
    Some(context.to_string())
}

fn note_signature(sections: &mut Vec<String>, body: &str) {
    let trimmed = body.trim();
    if is_signature(trimmed) {
        remember(sections, trimmed.to_string());
    }
}

fn remember(sections: &mut Vec<String>, section: String) {
    if !sections.contains(&section) {
        sections.push(section);
    }
}

fn is_signature(line: &str) -> bool {
    const MODIFIERS: &[&str] = &[
        "pub ",
        "pub(crate) ",
        "pub(super) ",
        "public ",
        "private ",
        "protected ",
        "export ",
        "default ",
        "async ",
        "static ",
        "final ",
        "abstract ",
    ];
    const LEADS: &[&str] = &[
        "import ",
        "from ",
        "use ",
        "#include",
        "package ",
        "fn ",
        "def ",
        "class ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "func ",
        "function ",
        "type ",
        "interface ",
        "const ",
        "var ",
        "let ",
        "module ",
    ];
    let mut rest = line;
    while let Some(m) = MODIFIERS.iter().find(|m| rest.starts_with(**m)) {
        rest = &rest[m.len()..];
    }
    LEADS.iter().any(|lead| rest.starts_with(lead))
}

const MAX_CONTENT_COLS: usize = 16;
const MAX_CONTENT_CELL: usize = 160;

// A readable content preview of a record array: the short scalar fields as a literal table, capped
// narrow so the model reads it at face value. Values shown are exact; anything omitted resolves.
fn content_table(text: &str) -> Option<String> {
    let Value::Array(items) = serde_json::from_str::<Value>(text).ok()? else {
        return None;
    };
    let objects: Vec<&serde_json::Map<String, Value>> =
        items.iter().filter_map(Value::as_object).collect();
    if objects.len() < 2 || objects.len() * 2 < items.len() {
        return None;
    }

    let mut order: Vec<String> = Vec::new();
    let mut pop: HashMap<String, usize> = HashMap::new();
    let mut maxlen: HashMap<String, usize> = HashMap::new();
    let mut rows: Vec<HashMap<String, String>> = Vec::with_capacity(objects.len());
    for obj in &objects {
        let mut row = HashMap::new();
        collect_scalars(obj, &mut |k, v| {
            if is_boilerplate(&k) {
                return;
            }
            if !pop.contains_key(&k) {
                order.push(k.clone());
            }
            *pop.entry(k.clone()).or_insert(0) += 1;
            let m = maxlen.entry(k.clone()).or_insert(0);
            *m = (*m).max(v.chars().count());
            row.insert(k, v);
        });
        rows.push(row);
    }
    // A column is content or detail: drop whole columns whose values run long (bodies, blobs) so the
    // table stays short-valued and readable, rather than raggedly dropping a field's long cells.
    order.retain(|k| maxlen[k] <= MAX_CONTENT_CELL);
    if order.is_empty() {
        return None;
    }
    // Rank by coverage then distinct values, so identifying content (number, title, login) wins the
    // narrow column budget over near-constant fields (all-zero counts, all-false flags).
    let mut distinct: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
    for row in &rows {
        for (k, v) in row {
            distinct.entry(k).or_default().insert(v);
        }
    }
    order.sort_by(|a, b| {
        pop[b]
            .cmp(&pop[a])
            .then_with(|| distinct[b.as_str()].len().cmp(&distinct[a.as_str()].len()))
            .then_with(|| a.cmp(b))
    });
    order.truncate(MAX_CONTENT_COLS);

    let budget = (text.len() / 5).max(1024);
    let mut table = order.join("\t");
    let mut shown = 0;
    for row in &rows {
        let line = order
            .iter()
            .map(|c| row.get(c).map(String::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\t");
        if table.len() + 1 + line.len() > budget {
            break;
        }
        table.push('\n');
        table.push_str(&line);
        shown += 1;
    }
    // A short secret could sit in a content cell; drop to the values-omitted index if any detector fires.
    if !crate::detectorgate::detector_findings(text, &table).is_empty() {
        return None;
    }

    let more = objects.len().saturating_sub(shown);
    let tail = if more > 0 {
        format!(", {more} more rows")
    } else {
        String::new()
    };
    Some(format!(
        "[{} bytes offloaded, content table ({shown} of {} rows{tail}, {} fields), call resolve for omitted fields and full detail]\n{table}",
        text.len(),
        objects.len(),
        order.len(),
    ))
}

// Emits (key, value) for each scalar field, descending one level into scalar sub-objects (dotted key).
fn collect_scalars(obj: &serde_json::Map<String, Value>, out: &mut impl FnMut(String, String)) {
    for (k, v) in obj {
        match v {
            Value::Object(sub) => {
                for (k2, v2) in sub {
                    if let Some(s) = render_scalar(v2) {
                        out(format!("{k}.{k2}"), s);
                    }
                }
            }
            _ => {
                if let Some(s) = render_scalar(v) {
                    out(k.clone(), s);
                }
            }
        }
    }
}

fn render_scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.replace(['\t', '\n'], " ")),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn is_boilerplate(key: &str) -> bool {
    const MARKERS: &[&str] = &[
        "url", "href", "_links", "node_id", "gravatar", "avatar", "sha",
    ];
    let k = key.to_ascii_lowercase();
    MARKERS.iter().any(|m| k.contains(m))
}

// Index by shape: field names, no values, so a preview is never mistaken for the data. Scalars: None.
fn json_index(text: &str, budget: usize) -> Option<String> {
    match serde_json::from_str::<Value>(text).ok()? {
        Value::Array(items) => {
            let n = items.len();
            match items.iter().find_map(Value::as_object) {
                Some(first) => {
                    let keys: Vec<String> = first.keys().cloned().collect();
                    Some(format!(
                        "array of {n} objects, fields: {}",
                        key_list(&keys, budget)
                    ))
                }
                None => Some(format!("array of {n} values")),
            }
        }
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            Some(format!(
                "object with {} fields: {}",
                keys.len(),
                key_list(&keys, budget)
            ))
        }
        _ => None,
    }
}

fn key_list(keys: &[String], budget: usize) -> String {
    let mut out = String::new();
    for (i, key) in keys.iter().enumerate() {
        let sep = if i == 0 { "" } else { ", " };
        if !out.is_empty() && out.len() + sep.len() + key.len() > budget {
            out.push_str(&format!(", +{} more", keys.len() - i));
            break;
        }
        out.push_str(sep);
        out.push_str(key);
    }
    out
}

// The preview an offload of this block would show, or None if too small to offload.
pub fn preview_if_offloaded(raw: &str) -> Option<String> {
    let canonical = match serde_json::from_str::<Value>(raw) {
        Ok(value) => canonicalize(&value),
        Err(_) => raw.to_string(),
    };
    if canonical.len() < MIN_OFFLOAD_BYTES {
        return None;
    }
    Some(preview(&canonical))
}

// True only when the offload preview would show real values (a record content table), so the agent
// can often answer without resolving and the measured REOPEN_PRIOR holds. A shape-only or head
// preview forces a resolve (reopen prior near 1), so eviction saves nothing and inline should win.
pub fn covers_content(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let canonical = canonicalize(&value);
    canonical.len() >= MIN_OFFLOAD_BYTES && content_table(&canonical).is_some()
}

// The content-covering preview, or None. Fuses covers_content and preview_if_offloaded into one
// parse + canonicalize for the Auto gate, which else pays for both passes on every inline block.
pub fn covering_preview(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    let canonical = canonicalize(&value);
    if canonical.len() < MIN_OFFLOAD_BYTES {
        return None;
    }
    content_table(&canonical)
}

fn hash(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_object() -> String {
        let rows: Vec<String> = (0..200)
            .map(|i| format!(r#""k{i}":"value number {i} for record {i}""#))
            .collect();
        format!("{{{}}}", rows.join(","))
    }

    fn big_array() -> String {
        let items: Vec<String> = (0..40)
            .map(|i| format!(
                r#"{{"number":{i},"state":"open","title":"fix bug {i}","html_url":"https://github.com/o/r/pull/{i}","user":{{"login":"dev{i}","id":{}}}}}"#,
                1000 + i
            ))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn a_record_array_previews_as_a_readable_content_table() {
        let store = Store::default();
        let raw = big_array();
        let out = store.offload(&raw).unwrap();
        assert!(out.stub.contains("content table"));
        // Real content is shown as a literal table so the agent answers common lookups inline.
        assert!(out.stub.contains("title") && out.stub.contains("fix bug 7"));
        assert!(out.stub.contains("dev7"));
        // No fragment/index/hoist codec: every shown value is literal and model-readable.
        assert!(!out.stub.contains("affix\t") && !out.stub.contains("\ndict\t"));
        // Boilerplate url columns stay omitted, recoverable through resolve.
        assert!(!out.stub.contains("html_url"));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn a_large_block_offloads_and_resolves_to_exact_bytes() {
        let store = Store::default();
        let raw = big_object();
        let out = store.offload(&raw).unwrap();
        assert!(out.stub.len() < raw.len());
        assert!(out.stub.contains(&out.marker));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn a_tampered_on_disk_blob_does_not_resolve() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("sw_offload_integrity_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = big_object();

        let writer = Store::persistent(dir.clone(), Duration::from_secs(3600));
        let marker = writer.offload(&raw).unwrap().marker;
        let id = marker_id(&marker).unwrap();
        let path = dir.join(format!("{id}.blob"));

        // A cold store resolves the uncorrupted blob from disk.
        let reader = Store::persistent(dir.clone(), Duration::from_secs(3600));
        assert_eq!(reader.resolve(&marker).as_deref(), Some(raw.as_str()));

        // Tamper the blob (same length, not a size check); a cold store must refuse it.
        let corrupted: String = raw.chars().rev().collect();
        assert_eq!(corrupted.len(), raw.len());
        std::fs::write(&path, &corrupted).unwrap();
        let cold = Store::persistent(dir.clone(), Duration::from_secs(3600));
        assert_eq!(
            cold.resolve(&marker),
            None,
            "a tampered blob must not resolve"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_callback_store_offloads_and_resolves_through_host_put_and_get() {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        // A host-supplied backend (here a shared map) receives the bytes; secondwind keeps the rest.
        let backend = Arc::new(Mutex::new(HashMap::<String, String>::new()));
        let (put_backend, get_backend) = (backend.clone(), backend.clone());
        let store = CallbackStore::new(
            move |id, val| {
                put_backend
                    .lock()
                    .unwrap()
                    .insert(id.to_string(), val.to_string());
                true
            },
            move |id| get_backend.lock().unwrap().get(id).cloned(),
        );

        let raw = big_object();
        let out = store.offload(&raw).unwrap();
        assert!(out.marker.starts_with("<<swload:"));
        assert_eq!(
            backend.lock().unwrap().len(),
            1,
            "the host backend received the block"
        );
        assert_eq!(
            store.resolve(&out.marker).as_deref(),
            Some(raw.as_str()),
            "resolves via host get"
        );
        assert!(
            store.covers(&out.marker, &raw),
            "coverage proof holds over the host backend"
        );
        assert!(
            store.offload("tiny").is_err(),
            "still refuses a block below the offload floor"
        );
    }

    #[test]
    fn structured_data_previews_as_a_field_index_with_no_values() {
        let store = Store::default();
        let raw = big_object();
        let out = store.offload(&raw).unwrap();
        assert!(out.stub.contains("json index"));
        assert!(out.stub.contains("object with 200 fields"));
        assert!(out.stub.contains("k0"));
        // A specific value must not leak into the preview, forcing the model to resolve.
        assert!(!out.stub.contains("value number 100 for record 100"));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn coverage_holds_for_the_offloaded_original() {
        let store = Store::default();
        let raw = big_object();
        let out = store.offload(&raw).unwrap();
        assert!(store.covers(&out.marker, &raw));
    }

    #[test]
    fn coverage_fails_for_a_marker_that_was_never_stored() {
        let store = Store::default();
        assert!(!store.covers("<<swload:deadbeefdeadbeef>>", "anything"));
    }

    #[test]
    fn small_blocks_are_not_offloaded() {
        let store = Store::default();
        assert!(matches!(
            store.offload("[1,2,3]"),
            Err(OffloadError::NotBigEnough)
        ));
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("sw-store-{}-{tag}-{n}", std::process::id()))
    }

    #[test]
    fn a_persistent_marker_resolves_from_a_fresh_store_after_restart() {
        let dir = scratch_dir("restart");
        let raw = big_object();

        let marker = {
            let writer = Store::persistent(&dir, Duration::from_secs(3600));
            writer.offload(&raw).unwrap().marker
        };

        // Fresh store with an empty hot map (stands in for a restart) still resolves off disk.
        let reader = Store::persistent(&dir, Duration::from_secs(3600));
        assert_eq!(reader.resolve(&marker).as_deref(), Some(raw.as_str()));
    }

    #[test]
    fn an_expired_marker_is_gone_and_its_file_is_swept() {
        let dir = scratch_dir("ttl");
        let marker = {
            let writer = Store::persistent(&dir, Duration::from_secs(3600));
            writer.offload(&big_object()).unwrap().marker
        };
        // A zero TTL makes every on-disk entry read as expired.
        let reader = Store::persistent(&dir, Duration::ZERO);
        assert!(reader.resolve(&marker).is_none());
        let id = marker_id(&marker).unwrap();
        assert!(!dir.join(format!("{id}.blob")).exists());
    }

    #[test]
    fn a_diff_previews_as_changed_files_and_stays_recoverable() {
        let store = Store::default();
        let mut patch = String::new();
        for i in 0..20 {
            patch.push_str(&format!(
                "diff --git a/src/mod_{i}.rs b/src/mod_{i}.rs\n--- a/src/mod_{i}.rs\n+++ b/src/mod_{i}.rs\n@@ -1,3 +1,3 @@\n context\n-old value {i}\n+new value {i}\n"
            ));
        }
        let out = store.offload(&patch).unwrap();
        assert!(out.stub.contains("diff summary"));
        assert!(out.stub.contains("src/mod_0.rs  +1 -1"));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(patch.as_str()));
    }

    #[test]
    fn a_diff_summary_names_the_functions_that_changed() {
        let store = Store::default();
        let mut patch = String::from(
            "diff --git a/src/svc.rs b/src/svc.rs\n--- a/src/svc.rs\n+++ b/src/svc.rs\n@@ -10,4 +10,5 @@ pub fn handle_request(req: Req) -> Resp {\n",
        );
        for i in 0..200 {
            patch.push_str(&format!(" unchanged context line {i}\n"));
        }
        patch.push_str("-    let old = lookup(id);\n+    let fresh = lookup(id);\n+    pub fn added_helper() {}\n");
        let out = store.offload(&patch).unwrap();
        assert!(
            out.stub
                .contains("pub fn handle_request(req: Req) -> Resp {")
        );
        assert!(out.stub.contains("pub fn added_helper() {}"));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(patch.as_str()));
    }

    #[test]
    fn code_offload_shows_signatures_and_stays_recoverable() {
        let store = Store::default();
        let body: String = (0..40)
            .map(|i| {
                format!(
                    "pub fn handler_{i}(x: i32) -> i32 {{\n    let secret = {};\n    x + secret\n}}\n",
                    i * 7
                )
            })
            .collect();
        let out = store.offload(&body).unwrap();
        assert!(out.stub.contains("pub fn handler_0"));
        assert!(out.stub.contains("code outline"));
        assert!(store.covers(&out.marker, &body));
        assert_eq!(store.resolve(&out.marker).as_deref(), Some(body.as_str()));
    }
}
