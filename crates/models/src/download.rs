use std::ffi::c_char;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{CatalogEntry, Error, ModelId, Sha256};

const CHUNK: usize = 64 * 1024;
const REPORT_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Started { total: u64 },
    Advanced { done: u64 },
    Verifying,
    Installed,
    Failed(String),
    Cancelled,
}

pub struct Download {
    pub id: ModelId,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Download {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// wartet, bis der thread den abbruch bemerkt oder fertig ist.
    pub fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// lädt, verifiziert und installiert einen katalog-eintrag unter `support_root`. layout: `support_root/downloads/<id>/…` während des laufs, danach ein atomares umbenennen nach `support_root/models/<id>/`.
pub fn install(
    entry: &CatalogEntry,
    support_root: &Path,
    on: impl Fn(Progress) + Send + 'static,
) -> Download {
    let cancel = Arc::new(AtomicBool::new(false));
    let entry = entry.clone();
    let support_root = support_root.to_path_buf();
    let flag = Arc::clone(&cancel);
    let id = entry.id.clone();
    let handle = std::thread::Builder::new()
        .name(format!("tf-download-{}", entry.id))
        .spawn(move || {
            let outcome = run(&entry, &support_root, &flag, &on);
            match outcome {
                Ok(()) => on(Progress::Installed),
                Err(Error::Cancelled) => on(Progress::Cancelled),
                Err(e) => on(Progress::Failed(e.to_string())),
            }
        })
        .ok();
    Download { id, cancel, handle }
}

fn run(
    entry: &CatalogEntry,
    support_root: &Path,
    cancel: &AtomicBool,
    on: &impl Fn(Progress),
) -> Result<(), Error> {
    let target = crate::install_dir(support_root, entry);
    if target.join(".installed.json").exists() {
        return Err(Error::AlreadyInstalled(entry.id.clone()));
    }

    let total = entry.total_bytes();
    let downloads = support_root.join("downloads");
    let staging = downloads.join(&entry.id);
    fs::create_dir_all(&staging)?;
    fs::create_dir_all(target.parent().unwrap_or(&target))?;

    let already = existing_bytes(entry, &staging);
    check_disk_space(support_root, total.saturating_sub(already))?;

    on(Progress::Started { total });
    let connector =
        native_tls::TlsConnector::new().map_err(|e| Error::Http(format!("could not start TLS: {e}")))?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .tls_connector(std::sync::Arc::new(connector))
        .build();

    let mut done = 0u64;
    for file in &entry.files {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let final_path = staging.join(&file.path);
        if fs::metadata(&final_path).map(|meta| meta.len() == file.size).unwrap_or(false) {
            done = done.saturating_add(file.size);
            continue;
        }
        let part = part_path(&staging, &file.path);
        if let Some(parent) = part.parent() {
            fs::create_dir_all(parent)?;
        }
        download_file(
            &agent,
            &entry.url(file),
            &part,
            file.size,
            &file.sha256,
            cancel,
            &mut done,
            total,
            on,
        )?;
        fs::rename(&part, &final_path)?;
    }

    on(Progress::Verifying);
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }
    fs::rename(&staging, &target)
        .map_err(|e| Error::Http(format!("could not move the download into place: {e}")))?;

    if entry.is_runtime() {
        extract_archives(&target)?;
    }

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let metadata = serde_json::json!({
        "catalog_version": 2,
        "revision": entry.source.revision,
        "installed_at": stamp,
    });
    fs::write(target.join(".installed.json"), serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}

/// packt das runtime-archiv mit systemwerkzeugen aus und signiert die binaries ad-hoc, damit arm64 sie ausführen kann.
fn extract_archives(target: &Path) -> Result<(), Error> {
    let Ok(entries) = fs::read_dir(target) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let status = if name.ends_with(".zip") {
            std::process::Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&path).arg(target).status()
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            std::process::Command::new("/usr/bin/tar").arg("-xzf").arg(&path).arg("-C").arg(target).status()
        } else {
            continue;
        };
        match status {
            Ok(status) if status.success() => {
                let _ = fs::remove_file(&path);
            }
            Ok(status) => {
                return Err(Error::Http(format!("could not extract {name}: {status}")));
            }
            Err(e) => return Err(Error::Io(e)),
        }
    }
    sign_runtime(target);
    Ok(())
}

fn sign_runtime(target: &Path) {
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                stack.push(path);
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "llama-server" || name.ends_with(".dylib") {
                let _ = std::process::Command::new("/usr/bin/codesign")
                    .args(["--force", "--sign", "-"])
                    .arg(&path)
                    .status();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn download_file(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    size: u64,
    expected: &str,
    cancel: &AtomicBool,
    done: &mut u64,
    total: u64,
    on: &impl Fn(Progress),
) -> Result<(), Error> {
    let mut attempt = 0;
    loop {
        match once(agent, url, part, size, expected, cancel, *done, total, on) {
            Ok(()) => {
                *done = done.saturating_add(size);
                return Ok(());
            }
            Err(Error::HashMismatch { .. }) if attempt == 0 => {
                let _ = fs::remove_file(part);
                attempt += 1;
            }
            Err(other) => return Err(other),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn once(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    size: u64,
    expected: &str,
    cancel: &AtomicBool,
    base: u64,
    total: u64,
    on: &impl Fn(Progress),
) -> Result<(), Error> {
    let mut existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if existing > size {
        File::create(part)?;
        existing = 0;
    }

    if existing == size && size > 0 {
        on(Progress::Verifying);
        let actual = hash_file(part)?;
        return check(expected, actual, part);
    }

    let mut hasher = Sha256::new();
    if existing > 0 {
        let mut file = File::open(part)?;
        let mut buffer = vec![0u8; CHUNK];
        let mut left = existing;
        while left > 0 {
            let read = file.read(&mut buffer[..CHUNK.min(left as usize)])?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            left -= read as u64;
        }
    }

    let mut request = agent.get(url);
    if existing > 0 {
        request = request.set("Range", &format!("bytes={existing}-"));
    }
    let response = match request.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(416, _)) => {
            on(Progress::Verifying);
            let actual = hash_file(part)?;
            return check(expected, actual, part);
        }
        Err(ureq::Error::Status(code, response)) => {
            let _ = response.into_string();
            return Err(Error::Http(format!("HTTP {code} for {url}")));
        }
        Err(ureq::Error::Transport(transport)) => {
            return Err(Error::Http(format!("{url}: {transport}")));
        }
    };

    let resume = response.status() == 206 && existing > 0;
    let mut file = if resume {
        OpenOptions::new().append(true).open(part)?
    } else {
        if existing > 0 {
            hasher = Sha256::new();
        }
        existing = 0;
        File::create(part)?
    };
    let mut written = existing;
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut last_report = Instant::now() - REPORT_INTERVAL;
    on(Progress::Advanced { done: base.saturating_add(written) });

    loop {
        if cancel.load(Ordering::Relaxed) {
            file.flush()?;
            return Err(Error::Cancelled);
        }
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        written += read as u64;
        if last_report.elapsed() >= REPORT_INTERVAL {
            on(Progress::Advanced { done: base.saturating_add(written).min(total) });
            last_report = Instant::now();
        }
    }
    file.flush()?;
    drop(file);

    if written != size {
        return Err(Error::Http(format!("{} is {written} bytes, expected {size}", part.display())));
    }
    on(Progress::Verifying);
    let actual = Sha256::hex(&hasher.finish());
    check(expected, actual, part)
}

fn check(expected: &str, actual: String, path: &Path) -> Result<(), Error> {
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(Error::HashMismatch { path: path.display().to_string(), expected: expected.into(), actual })
    }
}

fn hash_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Sha256::hex(&hasher.finish()))
}

fn part_path(staging: &Path, relative: &str) -> PathBuf {
    staging.join(format!("{relative}.part"))
}

fn existing_bytes(entry: &CatalogEntry, staging: &Path) -> u64 {
    entry
        .files
        .iter()
        .map(|file| {
            let part = fs::metadata(part_path(staging, &file.path)).map(|m| m.len()).unwrap_or(0);
            let complete = fs::metadata(staging.join(&file.path)).map(|m| m.len()).unwrap_or(0);
            part.max(complete).min(file.size)
        })
        .sum()
}

#[repr(C)]
struct StatFs {
    f_bsize: u32,
    f_iosize: i32,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_fsid: [i32; 2],
    f_owner: u32,
    f_type: u32,
    f_flags: u32,
    f_fssubtype: u32,
    f_fstypename: [u8; 16],
    f_mntonname: [u8; 1024],
    f_mntfromname: [u8; 1024],
    f_flags_ext: u32,
    f_reserved: [u32; 7],
}

unsafe extern "C" {
    fn statfs(path: *const c_char, buffer: *mut StatFs) -> i32;
}

fn available_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    let path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<StatFs>::zeroed();
    // SAFETY: `path` ist ein gültiger c-string und `stat` groß genug für `struct statfs`.
    let status = unsafe { statfs(path.as_ptr(), stat.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: statfs war erfolgreich und hat die struktur initialisiert.
    let stat = unsafe { stat.assume_init() };
    Some(stat.f_bavail.saturating_mul(u64::from(stat.f_bsize)))
}

fn check_disk_space(root: &Path, needed: u64) -> Result<(), Error> {
    let required = needed.saturating_add(needed / 10);
    if let Some(available) = available_bytes(root)
        && available < required
    {
        return Err(Error::DiskFull { needed: required, available });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_paths_keep_the_original_extension() {
        let path = part_path(Path::new("/tmp/x"), "Encoder.mlmodelc/weights/weight.bin");
        assert!(path.ends_with("weight.bin.part"));
    }

    #[test]
    fn disk_space_is_readable() {
        assert!(available_bytes(Path::new("/")).unwrap_or(0) > 0);
    }
}
