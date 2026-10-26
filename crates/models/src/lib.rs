//! die crate ist unabhängig von der ui: downloads melden fortschritt über einen callback, die darstellung entscheidet der aufrufer.

mod catalog;
mod download;
mod registry;
mod sha256;

pub use catalog::{Catalog, CatalogEntry, CatalogFile, Source};
pub use download::{Download, Progress, install};
pub use registry::{Installed, cleanup_downloads, scan, scan_runtimes, uninstall};
pub use sha256::Sha256;

use std::path::{Path, PathBuf};

pub type ModelId = String;

/// wohin ein installierter eintrag gehört. runtimes haben einen eigenen baum, damit die modell-liste nie ggufs mit executables mischt.
pub fn install_dir(support_root: &Path, entry: &CatalogEntry) -> PathBuf {
    if entry.is_runtime() {
        support_root.join("runtimes").join("llama.cpp").join(&entry.id)
    } else {
        support_root.join("models").join(&entry.id)
    }
}

pub fn runtime_root(support_root: &Path) -> PathBuf {
    support_root.join("runtimes").join("llama.cpp")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog: {0}")]
    Json(#[from] serde_json::Error),
    #[error("download: {0}")]
    Http(String),
    #[error("{path} failed verification: expected {expected}, got {actual}")]
    HashMismatch { path: String, expected: String, actual: String },
    #[error("not enough disk space: need {needed} bytes, {available} available")]
    DiskFull { needed: u64, available: u64 },
    #[error("{0} is already installed")]
    AlreadyInstalled(String),
    #[error("cancelled")]
    Cancelled,
    #[error("unknown model: {0}")]
    UnknownModel(String),
}
