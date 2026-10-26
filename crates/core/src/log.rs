//! minimaler datei-logger. kein `tracing`, das kostet mehr als dieses ganze modul.

use std::fs::{self, File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERR ",
        }
    }
}

struct Sink {
    file: File,
    path: PathBuf,
}

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

pub fn init(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    rotate_if_needed(path);
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    let _ = SINK.set(Mutex::new(Some(Sink { file, path: path.to_path_buf() })));
    Ok(())
}

pub fn write(level: Level, module: &str, message: &str) {
    let line = format!("{} {} {module}: {message}\n", timestamp(), level.label());
    if std::io::stderr().is_terminal() {
        eprint!("{line}");
    }
    let Some(sink) = SINK.get() else {
        return;
    };
    let Ok(mut guard) = sink.lock() else {
        return;
    };
    let Some(sink) = guard.as_mut() else {
        return;
    };
    if sink.file.metadata().map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        rotate(sink);
    }
    let _ = sink.file.write_all(line.as_bytes());
}

fn rotate_if_needed(path: &Path) {
    if fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let rotated = path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(path, rotated);
    }
}

fn rotate(sink: &mut Sink) {
    let _ = sink.file.flush();
    let rotated = sink.path.with_extension("log.1");
    let _ = fs::remove_file(&rotated);
    let _ = fs::rename(&sink.path, &rotated);
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&sink.path) {
        sink.file = file;
    }
}

fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// howard hinnants civil-from-days-algorithmus.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Info, module_path!(), &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Warn, module_path!(), &format!($($arg)*))
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::log::write($crate::log::Level::Error, module_path!(), &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_1970() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }
}
