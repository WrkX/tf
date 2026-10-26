use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::mode::{Mode, Refine, default_modes, summary_refine};
use crate::text::Vocabulary;
use crate::trigger::{Style, TriggerKey};
use tf_audio::vad::Sensitivity;

pub const DEFAULT_MODEL: &str = "parakeet-tdt-0.6b-v3";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub app: AppConfig,
    pub trigger: Trigger,
    pub insertion: Insertion,
    pub audio: AudioConfig,
    pub engine: EngineConfig,
    pub history: History,
    #[serde(default = "default_modes")]
    pub modes: Vec<Mode>,
    #[serde(default = "default_mode_id", rename = "default_mode", alias = "default_mode_id")]
    pub default_mode_id: String,
    pub llm: LlmConfig,
    pub vocabulary: Vocabulary,
    pub meetings: Meetings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    pub key: TriggerKey,
    pub style: Style,
    pub hold_threshold_ms: u32,
    /// wechselt den modus während einer aufnahme. im leerlauf ignoriert. ohne taste zyklt tab.
    pub mode_switch_key: Option<TriggerKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Insertion {
    pub method: InsertionMethod,
    /// überschreibungen pro app, schlüssel ist die bundle-id.
    pub overrides: BTreeMap<String, InsertionMethod>,
    pub append_space: bool,
    pub restore_clipboard: bool,
    pub press_return: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// coreaudio-geräte-uid, oder system-standard bei `None`.
    pub device: Option<String>,
    pub vad: Sensitivity,
    pub silence_removal: bool,
    pub normalize: bool,
    pub sounds: bool,
    pub sound_style: SoundStyle,
    /// wiedergabelautstärke der effekte, `0.0..=1.0`.
    pub sound_volume: f32,
    pub max_recording_secs: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SoundStyle {
    Simple,
    #[default]
    Classic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub default: String,
    /// entlädt das sprachmodell nach so vielen leerlauf-sekunden; 0 lässt es geladen.
    pub unload_after_secs: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub show_in_dock: bool,
    pub processing_glow: bool,
    /// leuchtfarbe als `#RRGGBB`.
    pub glow_color: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { show_in_dock: false, processing_glow: false, glow_color: "#4F8CFF".into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub store: HistoryLevel,
    pub retention_days: Option<u32>,
    pub audio_retention_days: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Meetings {
    pub detect: bool,
    pub capture: CaptureMode,
    pub summary: Refine,
    /// schreibt beide spuren während der aufnahme als aac.
    pub keep_audio: bool,
    /// nutzt den voice-processing-mikrofonpfad, wenn speaker erkannt werden.
    pub echo_cancellation: bool,
    pub identify_speakers: bool,
    pub max_speakers: u8,
    /// kosinus-distanz, ab der zwei stimm-cluster getrennt bleiben.
    pub speaker_threshold: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CaptureMode {
    /// alles system-audio außer tf selbst.
    #[default]
    All,
    /// nur die ausgabe der erkannten meeting-app.
    App,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub endpoints: Vec<Endpoint>,
    pub local: LocalLlmConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Endpoint {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// nie der key selbst; der key liegt in der keychain.
    pub has_key: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalLlmConfig {
    pub keep_alive_minutes: u32,
    pub context: u32,
}

// TODO: config.json hat noch kein version-feld, migrationen müssten bisher raten.
fn default_mode_id() -> String {
    crate::mode::RAW_ID.to_owned()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HistoryLevel {
    Off,
    #[default]
    Text,
    Audio,
}

impl HistoryLevel {
    pub fn stores_text(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn stores_audio(self) -> bool {
        matches!(self, Self::Audio)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum InsertionMethod {
    #[default]
    Paste,
    Type,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            app: AppConfig::default(),
            trigger: Trigger::default(),
            insertion: Insertion::default(),
            audio: AudioConfig::default(),
            engine: EngineConfig::default(),
            history: History::default(),
            modes: default_modes(),
            default_mode_id: default_mode_id(),
            llm: LlmConfig::default(),
            vocabulary: Vocabulary::default(),
            meetings: Meetings::default(),
        }
    }
}

impl Default for Trigger {
    fn default() -> Self {
        Self {
            key: TriggerKey::default(),
            style: Style::default(),
            hold_threshold_ms: 300,
            mode_switch_key: None,
        }
    }
}

impl Default for Insertion {
    fn default() -> Self {
        Self {
            method: InsertionMethod::default(),
            overrides: BTreeMap::new(),
            append_space: true,
            restore_clipboard: true,
            press_return: false,
        }
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: None,
            vad: Sensitivity::default(),
            silence_removal: true,
            normalize: false,
            sounds: true,
            sound_style: SoundStyle::default(),
            sound_volume: 0.25,
            max_recording_secs: 600,
        }
    }
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { default: DEFAULT_MODEL.to_owned(), unload_after_secs: 0 }
    }
}

impl Default for History {
    fn default() -> Self {
        Self { store: HistoryLevel::default(), retention_days: None, audio_retention_days: Some(30) }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![Endpoint {
                id: "ollama".to_owned(),
                name: "Ollama".to_owned(),
                base_url: "http://127.0.0.1:11434/v1".to_owned(),
                has_key: false,
            }],
            local: LocalLlmConfig::default(),
        }
    }
}

impl Default for Meetings {
    fn default() -> Self {
        Self {
            detect: true,
            capture: CaptureMode::default(),
            summary: summary_refine(),
            keep_audio: true,
            echo_cancellation: true,
            identify_speakers: false,
            max_speakers: 8,
            speaker_threshold: 0.70,
        }
    }
}

impl Default for LocalLlmConfig {
    fn default() -> Self {
        Self { keep_alive_minutes: 10, context: 4096 }
    }
}

pub struct Loaded {
    pub config: Config,
    /// gesetzt, wenn eine kaputte datei beiseitegelegt und der default geschrieben wurde.
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("config: {0}")]
    Parse(#[from] serde_json::Error),
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&text)?;
        config.normalize_modes();
        Ok(config)
    }

    /// lädt die config und legt eine kaputte datei beiseite, damit der nächste start sauber ist.
    pub fn load_or_backup(path: &Path) -> Loaded {
        match Self::load(path) {
            Ok(config) => Loaded { config, warning: None },
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Loaded { config: Self::default(), warning: None }
            }
            Err(e) => {
                let backup = broken_name(path);
                let _ = fs::rename(path, &backup);
                let config = Self::default();
                let _ = config.save(path);
                Loaded {
                    warning: Some(format!(
                        "{e}. The old file was moved to {} and defaults were restored.",
                        backup.display()
                    )),
                    config,
                }
            }
        }
    }

    /// schreibt atomar: `config.json.tmp`, fsync, rename.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let mut file = File::create(&tmp)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn broken_name(path: &Path) -> PathBuf {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("config.json");
    path.with_file_name(format!("{name}.broken-{stamp}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tf-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_round_trip() {
        let dir = temp_dir();
        let path = dir.join("config.json");
        Config::default().save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.trigger.key, TriggerKey::RightOption);
        assert_eq!(loaded.trigger.style, Style::Hybrid);
        assert_eq!(loaded.insertion.method, InsertionMethod::Paste);
        assert!(loaded.insertion.append_space);
        assert!(loaded.audio.sounds);
        assert_eq!(loaded.engine.default, DEFAULT_MODEL);
        assert_eq!(loaded.history.store, HistoryLevel::Text);
        assert_eq!(loaded.history.audio_retention_days, Some(30));
        assert_eq!(loaded.modes.len(), 6);
        assert_eq!(loaded.default_mode_id, "raw");
        assert_eq!(loaded.llm.local.keep_alive_minutes, 10);
        assert_eq!(loaded.llm.endpoints.len(), 1);
        assert!(loaded.meetings.detect);
        assert_eq!(loaded.meetings.capture, CaptureMode::All);
        assert_eq!(loaded.meetings.summary.prompt, crate::mode::MEETING_SUMMARY_PROMPT);
        assert!(!loaded.meetings.identify_speakers);
        assert_eq!(loaded.meetings.max_speakers, 8);
        assert!((loaded.meetings.speaker_threshold - 0.70).abs() < f32::EPSILON);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_mode_uses_the_documented_key() {
        let json = serde_json::to_string(&Config::default()).unwrap();
        assert!(json.contains("\"default_mode\":\"raw\""));
        assert!(!json.contains("default_mode_id"));
        let parsed: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.default_mode_id, "raw");
    }

    #[test]
    fn older_configs_get_the_presets() {
        let config: Config = serde_json::from_str(r#"{"version":1,"engine":{"default":"x"}}"#).unwrap();
        assert_eq!(config.modes.len(), 6);
        assert_eq!(config.default_mode_id, "raw");
        assert!(config.vocabulary.terms.is_empty());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let config: Config = serde_json::from_str(r#"{"version":1,"future":{"a":1}}"#).unwrap();
        assert_eq!(config.version, 1);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = temp_dir();
        let path = dir.join("config.json");
        fs::write(&path, "{not json").unwrap();
        let loaded = Config::load_or_backup(&path);
        assert!(loaded.warning.is_some());
        assert!(Config::load(&path).is_ok());
        let broken = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".broken-"));
        assert!(broken);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_uses_defaults_without_writing() {
        let dir = temp_dir();
        let path = dir.join("config.json");
        let loaded = Config::load_or_backup(&path);
        assert!(loaded.warning.is_none());
        assert!(!path.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
