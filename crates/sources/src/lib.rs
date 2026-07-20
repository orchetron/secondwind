#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use secondwind_core::Trace;

pub mod claude_code;

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{source_id}: {path}:{line}: {detail}")]
    Drift {
        source_id: &'static str,
        path: PathBuf,
        line: usize,
        detail: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Default)]
pub struct ReadOutcome {
    pub traces: Vec<Trace>,
    pub skipped_record_types: BTreeMap<String, usize>,
}

pub trait Source {
    fn id(&self) -> &'static str;
    fn search_root(&self, home: &Path) -> PathBuf;
    fn discover(&self, home: &Path) -> io::Result<Vec<PathBuf>>;
    fn read(&self, path: &Path) -> Result<ReadOutcome, ReadError>;
}

pub trait Enricher {
    fn id(&self) -> &'static str;
    fn enrich(&self, trace: &mut Trace) -> usize;
}

pub fn all() -> Vec<Box<dyn Source>> {
    vec![Box::new(claude_code::ClaudeCode)]
}
