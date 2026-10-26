use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Debug)]
pub struct Installed {
    pub id: String,
    pub dir: PathBuf,
    pub installed_at: i64,
    pub size: u64,
}

#[derive(Deserialize, Serialize)]
struct Marker {
    #[serde(default)]
    catalog_version: u32,
    #[serde(default)]
    revision: String,
    #[serde(default)]
    installed_at: i64,
}

/// listet modelle, die vollständig installiert wurden (sie tragen `.installed.json`).
pub fn scan(models: &Path) -> Vec<Installed> {
    scan_root(models)
}

pub fn scan_runtimes(support_root: &Path) -> Vec<Installed> {
    scan_root(&crate::runtime_root(support_root))
}

fn scan_root(models: &Path) -> Vec<Installed> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(models) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker = dir.join(".installed.json");
        let Ok(text) = fs::read_to_string(&marker) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Marker>(&text) else {
            continue;
        };
        let Some(id) = dir.file_name().and_then(|name| name.to_str()).map(str::to_owned) else {
            continue;
        };
        out.push(Installed { id, size: dir_size(&dir), dir, installed_at: meta.installed_at });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn uninstall(models: &Path, id: &str) -> Result<(), Error> {
    let dir = models.join(id);
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    Ok(())
}

/// löscht zwischenverzeichnisse von downloads, die älter als `max_age` sind.
pub fn cleanup_downloads(downloads: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(downloads) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let modified = meta.modified().unwrap_or(now);
        if now.duration_since(modified).unwrap_or_default() > max_age {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name() != ".installed.json")
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => dir_size(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tf-models-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_ignores_partial_directories() {
        let root = temp_dir();
        let complete = root.join("complete");
        fs::create_dir_all(complete.join("weights")).unwrap();
        fs::write(complete.join("weights/a.bin"), b"1234").unwrap();
        fs::write(complete.join(".installed.json"), br#"{"installed_at":42}"#).unwrap();
        fs::create_dir_all(root.join("partial")).unwrap();
        fs::write(root.join("partial/a.bin"), b"12").unwrap();

        let found = scan(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "complete");
        assert_eq!(found[0].size, 4);
        assert_eq!(found[0].installed_at, 42);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_removes_the_directory() {
        let root = temp_dir();
        let dir = root.join("model");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a"), b"x").unwrap();
        uninstall(&root, "model").unwrap();
        assert!(!dir.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
