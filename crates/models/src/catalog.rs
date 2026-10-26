use serde::{Deserialize, Serialize};

/// der in die app eingebettete katalog. mit `cargo xtask catalog` neu erzeugen.
const EMBEDDED: &str = include_str!("../../../resources/catalog.json");

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    pub models: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub kind: String,
    pub engine: String,
    pub languages: Vec<String>,
    pub ram_mb: u32,
    pub source: Source,
    pub files: Vec<CatalogFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub repo: String,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

impl Catalog {
    pub fn embedded() -> Result<Self, crate::Error> {
        Ok(serde_json::from_str(EMBEDDED)?)
    }

    pub fn find(&self, id: &str) -> Option<&CatalogEntry> {
        self.models.iter().find(|model| model.id == id)
    }
}

impl CatalogEntry {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    /// hugging face löst dateien über diese url auf der gepinnten revision auf. runtimes kommen stattdessen aus einem gepinnten github-release.
    pub fn url(&self, file: &CatalogFile) -> String {
        if self.kind == "runtime" {
            format!(
                "https://github.com/{}/releases/download/{}/{}",
                self.source.repo, self.source.revision, file.path
            )
        } else {
            format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                self.source.repo, self.source.revision, file.path
            )
        }
    }

    pub fn is_runtime(&self) -> bool {
        self.kind == "runtime"
    }

    pub fn language_label(&self) -> String {
        match self.languages.len() {
            0 => "—".to_owned(),
            1 => self.languages[0].to_uppercase(),
            2..=4 => self.languages.iter().map(|l| l.to_uppercase()).collect::<Vec<_>>().join(", "),
            n => format!("{} languages", n),
        }
    }

    pub fn size_label(&self) -> String {
        format!("{:.0} MB", self.total_bytes() as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_is_valid() {
        let catalog = Catalog::embedded().unwrap();
        assert_eq!(catalog.version, 2);
        assert!(catalog.find("parakeet-tdt-0.6b-v3").is_some());
        assert!(catalog.find("parakeet-tdt-0.6b-v2").is_some());
        assert!(catalog.find("llama.cpp-b11669").is_some());
        assert!(catalog.find("qwen3-1.7b-q4").is_some());
        assert!(catalog.find("qwen3-4b-instruct-q4").is_some());
        assert!(catalog.find("speaker-diarization").is_some());
        for model in &catalog.models {
            assert!(!model.files.is_empty());
            assert!(model.files.iter().all(|f| f.sha256.len() == 64));
        }
    }

    #[test]
    fn urls_use_the_pinned_revision() {
        let catalog = Catalog::embedded().unwrap();
        let model = catalog.find("parakeet-tdt-0.6b-v3").unwrap();
        assert!(model.url(&model.files[0]).contains(&model.source.revision));
        let runtime = catalog.find("llama.cpp-b11669").unwrap();
        assert!(runtime.url(&runtime.files[0]).starts_with("https://github.com/"));
    }
}
