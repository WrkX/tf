use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn support_dir() -> PathBuf {
    home().join("Library/Application Support/tf")
}

pub fn config_path() -> PathBuf {
    support_dir().join("config.json")
}

pub fn history_path() -> PathBuf {
    support_dir().join("history.sqlite")
}

pub fn audio_dir() -> PathBuf {
    support_dir().join("audio")
}

pub fn meetings_dir() -> PathBuf {
    support_dir().join("meetings")
}

pub fn meeting_dir(id: i64) -> PathBuf {
    meetings_dir().join(id.to_string())
}

/// der relative pfad, der in `meetings.audio_me` / `audio_them` steht.
pub fn meeting_audio_file(id: i64, speaker: &str) -> PathBuf {
    PathBuf::from("meetings").join(id.to_string()).join(format!("{speaker}.m4a"))
}

pub fn log_dir() -> PathBuf {
    home().join("Library/Logs/tf")
}

pub fn log_path() -> PathBuf {
    log_dir().join("tf.log")
}

pub fn llama_log_path() -> PathBuf {
    log_dir().join("llama-server.log")
}

pub fn models_dir() -> PathBuf {
    support_dir().join("models")
}

pub fn model_dir(id: &str) -> PathBuf {
    models_dir().join(id)
}

/// der relative audio-pfad, der in `takes.audio_file` steht.
pub fn audio_file(unix_ms: i64, id: i64) -> PathBuf {
    let (year, month, _) = civil_date(unix_ms);
    PathBuf::from(format!("{year:04}/{month:02}/{id}.m4a"))
}

/// utc-datum aus unix-millisekunden. ordnernamen brauchen nur eine stabile, lesbare gruppierung, keine lokale zeit.
fn civil_date(unix_ms: i64) -> (i32, u32, u32) {
    let days = unix_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    if month <= 2 {
        year += 1;
    }
    (year as i32, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_paths_group_by_utc_month() {
        assert_eq!(audio_file(1_792_800_000_000, 42), PathBuf::from("2026/10/42.m4a"));
    }
}
