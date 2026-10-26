use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

mod fixture;

const APP: &str = "tf";
const SIZE_LIMIT: u64 = 10 * 1024 * 1024;
const MODEL_ID: &str = "parakeet-tdt-0.6b-v3";

struct CatalogSpec {
    id: &'static str,
    repo: &'static str,
    revision: &'static str,
    name: &'static str,
    summary: &'static str,
    prefixes: &'static [&'static str],
    vocab: &'static str,
    languages: &'static [&'static str],
    ram_mb: u32,
}

const CATALOG_SPECS: [CatalogSpec; 2] = [
    CatalogSpec {
        id: "parakeet-tdt-0.6b-v2",
        repo: "FluidInference/parakeet-tdt-0.6b-v2-coreml",
        revision: "ee09c569f73759e6d44c9bd16766f477b2b36d39",
        name: "Parakeet v2",
        summary: "Fast English dictation",
        prefixes: &[
            "Preprocessor.mlmodelc/",
            "Encoder.mlmodelc/",
            "Decoder.mlmodelc/",
            "JointDecision.mlmodelc/",
        ],
        vocab: "parakeet_vocab.json",
        languages: &["en"],
        ram_mb: 512,
    },
    CatalogSpec {
        id: "parakeet-tdt-0.6b-v3",
        repo: "FluidInference/parakeet-tdt-0.6b-v3-coreml",
        revision: "7dd20fe6b1797d35f5e3307e8b1732d9a178edfe",
        name: "Parakeet v3",
        summary: "Fast multilingual (25 European languages)",
        prefixes: &[
            "Preprocessor.mlmodelc/",
            "Encoder.mlmodelc/",
            "Decoder.mlmodelc/",
            "JointDecisionv3.mlmodelc/",
        ],
        vocab: "parakeet_vocab.json",
        languages: &[
            "en", "de", "fr", "es", "it", "nl", "pl", "pt", "ru", "uk", "cs", "da", "sv", "no", "fi", "el",
            "hu", "ro", "sk", "sl", "hr", "bg", "et", "lv", "lt",
        ],
        ram_mb: 512,
    },
];

type Result<T = ()> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let task = env::args().nth(1);
    let result = match task.as_deref() {
        Some("bundle") => bundle().map(|app| report_size(&app)),
        Some("run") => bundle().and_then(|app| run(&app)),
        Some("fetch-model") => fetch_model(),
        Some("catalog") => catalog(),
        Some("diarize-fixture") => fixture::run(env::args().nth(2)),
        _ => Err("usage: cargo xtask <bundle|run|fetch-model|catalog|diarize-fixture>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn bundle() -> Result<PathBuf> {
    let root = workspace_root();
    cmd(Command::new("cargo").args(["build", "--release", "-p", "tf-app"]).current_dir(&root))?;

    let app = root.join("dist").join(format!("{APP}.app"));
    let contents = app.join("Contents");
    let _ = fs::remove_dir_all(&app);
    fs::create_dir_all(contents.join("MacOS")).map_err(|e| e.to_string())?;

    let plist = fs::read_to_string(root.join("resources/Info.plist")).map_err(|e| e.to_string())?;
    let plist = plist.replace("$VERSION", env!("CARGO_PKG_VERSION"));
    fs::write(contents.join("Info.plist"), plist).map_err(|e| e.to_string())?;
    fs::copy(root.join("target/release").join(APP), contents.join("MacOS").join(APP))
        .map_err(|e| e.to_string())?;

    sign(&app)?;
    Ok(app)
}

/// signiert mit einer stabilen identität, damit macos die datenschutz-freigaben über builds hinweg behält.
fn sign(app: &Path) -> Result {
    let identity = env::var("TF_SIGN_IDENTITY").ok().or_else(find_identity).unwrap_or_else(|| {
        eprintln!("warning: no signing identity found, ad-hoc signing (permissions reset on every build)");
        "-".into()
    });
    let entitlements = workspace_root().join("resources/tf.entitlements");
    cmd(Command::new("codesign")
        .args(["--force", "--options", "runtime", "--sign", &identity, "--entitlements"])
        .arg(entitlements)
        .arg(app))
}

fn find_identity() -> Option<String> {
    let out = Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.contains("Apple Development") || l.contains("Developer ID Application"))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(str::to_owned)
}

fn report_size(app: &Path) {
    let bytes = dir_size(app);
    let mb = bytes as f64 / 1024.0 / 1024.0;
    let status = if bytes <= SIZE_LIMIT { "ok" } else { "OVER LIMIT" };
    println!("{} {mb:.2} MB ({status}, limit 10 MB)", app.display());
}

fn run(app: &Path) -> Result {
    report_size(app);
    cmd(&mut Command::new(app.join("Contents/MacOS").join(APP)))
}

/// installiert parakeet v3 aus dem gepinnten katalog. nur eine entwicklungs-hilfe; die app lädt modelle selbst herunter.
fn fetch_model() -> Result {
    let root = workspace_root();
    let text = fs::read_to_string(root.join("resources/catalog.json")).map_err(|e| e.to_string())?;
    let catalog: StoredCatalog = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == MODEL_ID)
        .ok_or_else(|| format!("{MODEL_ID} is missing from the catalog"))?;

    let install = app_support().join("models").join(MODEL_ID);
    fs::create_dir_all(&install).map_err(|e| e.to_string())?;
    let total = model.files.len();
    for (index, file) in model.files.iter().enumerate() {
        let destination = install.join(&file.path);
        if fs::metadata(&destination).map(|meta| meta.len() == file.size).unwrap_or(false) {
            println!("[{}/{total}] {} (cached)", index + 1, file.path);
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let part = destination.with_file_name(format!(
            "{}.part",
            destination.file_name().and_then(|name| name.to_str()).unwrap_or("model")
        ));
        println!("[{}/{total}] {}", index + 1, file.path);
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            model.source.repo, model.source.revision, file.path
        );
        cmd(Command::new("curl")
            .args(["--fail", "--location", "--retry", "3", "--silent", "--show-error", "--output"])
            .arg(&part)
            .arg(url))?;
        let actual = sha256_file(&part)?;
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            let _ = fs::remove_file(&part);
            return Err(format!("{} failed verification", file.path));
        }
        fs::rename(&part, &destination).map_err(|e| e.to_string())?;
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let marker = serde_json::json!({
        "catalog_version": catalog.version,
        "revision": model.source.revision,
        "installed_at": stamp,
    });
    fs::write(
        install.join(".installed.json"),
        serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!("installed {MODEL_ID} into {}", install.display());
    Ok(())
}

#[derive(serde::Deserialize)]
struct StoredCatalog {
    version: u32,
    models: Vec<StoredModel>,
}

#[derive(serde::Deserialize)]
struct StoredModel {
    id: String,
    source: StoredSource,
    files: Vec<StoredFile>,
}

#[derive(serde::Deserialize)]
struct StoredSource {
    repo: String,
    revision: String,
}

#[derive(serde::Deserialize)]
struct StoredFile {
    path: String,
    size: u64,
    sha256: String,
}

fn sha256_file(path: &Path) -> Result<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .map_err(|e| format!("could not run shasum: {e}"))?;
    if !output.status.success() {
        return Err("shasum failed".into());
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| "shasum returned nothing".into())
}

#[derive(serde::Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    lfs: Option<LfsEntry>,
}

#[derive(serde::Deserialize)]
struct LfsEntry {
    oid: String,
}

/// erzeugt `resources/catalog.json` neu aus den gepinnten hugging-face-revisionen.
fn catalog() -> Result {
    let root = workspace_root();
    let mut models = Vec::new();
    for spec in &CATALOG_SPECS {
        let url =
            format!("https://huggingface.co/api/models/{}/tree/{}?recursive=1", spec.repo, spec.revision);
        let tree = curl_text(&url)?;
        let entries: Vec<TreeEntry> =
            serde_json::from_str(&tree).map_err(|e| format!("could not parse {}: {e}", spec.repo))?;

        let mut files = Vec::new();
        for entry in entries.iter().filter(|entry| entry.kind.as_deref() != Some("directory")) {
            let needed =
                spec.prefixes.iter().any(|prefix| entry.path.starts_with(prefix)) || entry.path == spec.vocab;
            if !needed {
                continue;
            }
            let size = entry.size.unwrap_or(0);
            let sha256 = match &entry.lfs {
                Some(lfs) if !lfs.oid.is_empty() => lfs.oid.clone(),
                _ => {
                    let file_url = format!(
                        "https://huggingface.co/{}/resolve/{}/{}",
                        spec.repo, spec.revision, entry.path
                    );
                    let bytes = curl_bytes(&file_url)?;
                    if bytes.len() as u64 != size {
                        return Err(format!(
                            "{} is {} bytes in the listing but {} on disk",
                            entry.path,
                            size,
                            bytes.len()
                        ));
                    }
                    sha256_bytes(&bytes)?
                }
            };
            files.push(serde_json::json!({
                "path": entry.path,
                "size": size,
                "sha256": sha256,
            }));
        }
        files.sort_by(|a, b| {
            a.get("path").and_then(|p| p.as_str()).cmp(&b.get("path").and_then(|p| p.as_str()))
        });
        if files.is_empty() {
            return Err(format!("no files matched for {}", spec.id));
        }
        let total: u64 = files.iter().filter_map(|f| f.get("size")).filter_map(|s| s.as_u64()).sum();
        println!("{}: {} files, {:.0} MB", spec.id, files.len(), total as f64 / 1e6);
        models.push(serde_json::json!({
            "id": spec.id,
            "name": spec.name,
            "summary": spec.summary,
            "kind": "stt",
            "engine": "parakeet",
            "languages": spec.languages,
            "ram_mb": spec.ram_mb,
            "source": { "repo": spec.repo, "revision": spec.revision },
            "files": files,
        }));
    }

    let path = root.join("resources/catalog.json");
    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(existing) = serde_json::from_str::<serde_json::Value>(&text)
    {
        for entry in existing["models"].as_array().into_iter().flatten() {
            if entry["kind"].as_str() != Some("stt") {
                models.push(entry.clone());
            }
        }
    }
    let catalog = serde_json::json!({ "version": 2, "models": models });
    let mut text = serde_json::to_string_pretty(&catalog).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(&path, text).map_err(|e| e.to_string())?;
    println!("wrote {}", path.display());
    Ok(())
}

fn curl_bytes(url: &str) -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("curl failed for {url}: {}", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(output.stdout)
}

fn sha256_bytes(bytes: &[u8]) -> Result<String> {
    let path = env::temp_dir().join(format!("tf-catalog-{}-{}", std::process::id(), bytes.len()));
    fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&path)
        .output()
        .map_err(|e| format!("could not run shasum: {e}"))?;
    let _ = fs::remove_file(&path);
    if !output.status.success() {
        return Err("shasum failed".into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(str::to_owned).ok_or_else(|| "shasum returned nothing".into())
}

fn curl_text(url: &str) -> Result<String> {
    let output = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error", url])
        .output()
        .map_err(|e| format!("could not run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("curl failed for {url}: {}", String::from_utf8_lossy(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("curl returned invalid UTF-8: {e}"))
}

fn app_support() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/tf")
}

fn dir_size(path: &Path) -> u64 {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(m) if m.is_dir() => dir_size(&entry.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

fn cmd(command: &mut Command) -> Result {
    let status = command.status().map_err(|e| format!("{command:?}: {e}"))?;
    status.success().then_some(()).ok_or_else(|| format!("{command:?} failed with {status}"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}
