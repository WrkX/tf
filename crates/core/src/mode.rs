//! modi: benannte nachbearbeitungs-konfigurationen, wählbar per hotkey, über die vordergrund-app oder aus der menüleiste.

use serde::{Deserialize, Serialize};

use crate::config::{Config, InsertionMethod};
use crate::trigger::TriggerKey;

pub type ModeId = String;

pub const RAW_ID: &str = "raw";

/// der verarbeitungsschritt, den ein modus nach der transkription anwendet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Refine {
    pub backend: Backend,
    /// eine katalog-gguf-id für `local`, ein server-modellname für `endpoint:<id>`.
    pub model: Option<String>,
    pub prompt: String,
    pub temperature: f32,
    pub timeout_ms: u32,
}

impl Default for Refine {
    fn default() -> Self {
        Self {
            backend: Backend::Apple,
            model: None,
            prompt: String::new(),
            temperature: 0.3,
            timeout_ms: 8000,
        }
    }
}

/// serialisiert als `apple`, `local`, `endpoint:<id>` oder `cloud:<provider>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Apple,
    Local,
    Endpoint(String),
    Cloud(String),
}

impl Backend {
    pub fn parse(value: &str) -> Self {
        if value == "apple" {
            Self::Apple
        } else if value == "local" {
            Self::Local
        } else if let Some(id) = value.strip_prefix("endpoint:") {
            Self::Endpoint(id.to_owned())
        } else if let Some(provider) = value.strip_prefix("cloud:") {
            Self::Cloud(provider.to_owned())
        } else {
            Self::Apple
        }
    }

    pub fn as_str(&self) -> String {
        match self {
            Self::Apple => "apple".to_owned(),
            Self::Local => "local".to_owned(),
            Self::Endpoint(id) => format!("endpoint:{id}"),
            Self::Cloud(provider) => format!("cloud:{provider}"),
        }
    }

    pub fn cache_key(&self, model: Option<&str>) -> String {
        format!("{}:{}", self.as_str(), model.unwrap_or_default())
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }

    pub fn is_apple(&self) -> bool {
        matches!(self, Self::Apple)
    }
}

impl Serialize for Backend {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Backend {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::parse(&value))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Mode {
    pub id: ModeId,
    pub name: String,
    /// sf-symbol-name, angezeigt im menü und auf der pill.
    pub icon: String,
    pub engine: Option<String>,
    pub language: Option<String>,
    /// `None` heißt: modus ist raw, kein llm-schritt.
    pub refine: Option<Refine>,
    pub insertion: Option<InsertionMethod>,
    pub hotkey: Option<TriggerKey>,
    pub apps: Vec<String>,
    pub use_vocabulary: bool,
    pub quick_switch: bool,
}

impl Default for Mode {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            icon: "text.bubble".to_owned(),
            engine: None,
            language: None,
            refine: None,
            insertion: None,
            hotkey: None,
            apps: Vec::new(),
            use_vocabulary: true,
            quick_switch: true,
        }
    }
}

impl Mode {
    pub fn is_raw(&self) -> bool {
        self.refine().is_none()
    }

    pub fn refine(&self) -> Option<&Refine> {
        self.refine.as_ref().filter(|refine| !refine.prompt.trim().is_empty())
    }

    pub fn backend(&self) -> Option<&Backend> {
        self.refine().map(|refine| &refine.backend)
    }
}

pub const CLEAN_PROMPT: &str = "Rewrite the dictated text with filler words (um, uh, like, you know), false starts and\nself-corrections removed. When the speaker corrects themselves (\"Tuesday, no, Thursday\"),\nkeep only the correction. Fix punctuation and capitalization. Keep the speaker's wording,\nlanguage and meaning otherwise. Do not summarize.";

pub const MESSAGE_PROMPT: &str = "Turn the dictated text into a short, natural chat message. Casual tone, no greeting or\nsign-off unless dictated. Remove fillers and self-corrections. Keep the original language.";

pub const EMAIL_PROMPT: &str = "Turn the dictated text into a well-structured email body: a greeting if a recipient is\nmentioned, short paragraphs, and a closing line. Polite, clear and concise. Do not invent\nfacts, names or dates. Keep the original language. Do not add a subject line or signature.";

pub const NOTES_PROMPT: &str = "Turn the dictated text into concise bullet points (\"- \"). Group related points. Keep every\nfact; drop fillers. Keep the original language.";

pub const AI_PROMPT_PROMPT: &str = "Restructure the dictated text into a clear, self-contained prompt for an AI assistant.\nKeep every requirement and constraint the speaker mentioned. Remove fillers, false starts\nand repetition. Keep the original language. Do not answer or execute the prompt.";

pub const MEETING_SUMMARY_PROMPT: &str = "You are given a meeting transcript with speakers \"Me\" (the user) and \"Them\" (other\nparticipants).\nWrite Markdown with these sections:\n## Summary — 3–6 sentences.\n## Decisions — bullets, or \"None\".\n## Action items — \"- [ ] <task> (owner: Me/Them/name if stated, due: if stated)\".\n## Open questions — bullets, or \"None\".\nOnly use information from the transcript. Keep the transcript's language.";

pub fn summary_refine() -> Refine {
    Refine {
        prompt: MEETING_SUMMARY_PROMPT.to_owned(),
        temperature: 0.2,
        timeout_ms: 120_000,
        ..Refine::default()
    }
}

pub fn raw() -> Mode {
    Mode {
        id: RAW_ID.to_owned(),
        name: "Raw".to_owned(),
        icon: "waveform".to_owned(),
        refine: None,
        hotkey: Some(TriggerKey::Chord {
            keycode: 18,
            shift: false,
            control: true,
            option: true,
            command: false,
        }),
        use_vocabulary: false,
        ..Mode::default()
    }
}

/// die eingebauten presets. sie landen in einer frischen config, wo der nutzer sie bearbeiten kann.
pub fn presets() -> Vec<Mode> {
    vec![
        raw(),
        Mode {
            id: "clean".to_owned(),
            name: "Clean".to_owned(),
            icon: "wand.and.stars".to_owned(),
            refine: Some(Refine { prompt: CLEAN_PROMPT.to_owned(), ..Refine::default() }),
            hotkey: Some(TriggerKey::Chord {
                keycode: 19,
                shift: false,
                control: true,
                option: true,
                command: false,
            }),
            ..Mode::default()
        },
        Mode {
            id: "message".to_owned(),
            name: "Message".to_owned(),
            icon: "bubble.left.and.bubble.right".to_owned(),
            refine: Some(Refine { prompt: MESSAGE_PROMPT.to_owned(), ..Refine::default() }),
            apps: vec![
                "com.tinyspeck.slackmacgap".to_owned(),
                "com.apple.MobileSMS".to_owned(),
                "com.hnc.Discord".to_owned(),
            ],
            ..Mode::default()
        },
        Mode {
            id: "email".to_owned(),
            name: "Email".to_owned(),
            icon: "envelope".to_owned(),
            refine: Some(Refine { prompt: EMAIL_PROMPT.to_owned(), ..Refine::default() }),
            apps: vec!["com.apple.mail".to_owned(), "com.microsoft.Outlook".to_owned()],
            ..Mode::default()
        },
        Mode {
            id: "notes".to_owned(),
            name: "Notes".to_owned(),
            icon: "list.bullet".to_owned(),
            refine: Some(Refine { prompt: NOTES_PROMPT.to_owned(), ..Refine::default() }),
            apps: vec!["com.apple.Notes".to_owned(), "notion.id".to_owned()],
            ..Mode::default()
        },
        Mode {
            id: "prompt".to_owned(),
            name: "AI Prompt".to_owned(),
            icon: "sparkles".to_owned(),
            refine: Some(Refine { prompt: AI_PROMPT_PROMPT.to_owned(), ..Refine::default() }),
            ..Mode::default()
        },
    ]
}

pub fn default_modes() -> Vec<Mode> {
    presets()
}

pub fn preset(id: &str) -> Option<Mode> {
    presets().into_iter().find(|mode| mode.id == id)
}

impl Config {
    /// die konfigurierten modi, oder die presets wenn die liste leer ist.
    pub fn modes(&self) -> Vec<Mode> {
        if self.modes.is_empty() { presets() } else { self.modes.clone() }
    }

    pub fn mode(&self, id: &str) -> Option<Mode> {
        self.modes().into_iter().find(|mode| mode.id == id)
    }

    pub fn default_mode(&self) -> Mode {
        self.mode(&self.default_mode_id).or_else(|| self.mode(RAW_ID)).unwrap_or_else(raw)
    }

    /// legt fest, welchen modus ein take nutzt: zuerst ein hotkey des modus, dann der erste modus mit der app im vordergrund, dann der konfigurierte standard-modus.
    pub fn resolve_mode(&self, trigger: Option<&str>, bundle_id: Option<&str>) -> Mode {
        if let Some(id) = trigger
            && let Some(mode) = self.mode(id)
        {
            return mode;
        }
        if let Some(bundle) = bundle_id {
            let modes = self.modes();
            if let Some(mode) = modes.iter().find(|mode| mode.apps.iter().any(|app| app == bundle)) {
                return mode.clone();
            }
        }
        self.default_mode()
    }

    /// der nächste oder vorherige quick-switch-modus in listenreihenfolge, mit umlauf. bleibt auf `current`, wenn kein anderer wechsel erlaubt ist.
    pub fn cycle_mode(&self, current: &str, forward: bool) -> Mode {
        let modes = self.modes();
        let count = modes.len();
        let index = modes.iter().position(|mode| mode.id == current).unwrap_or(0);
        (1..count)
            .map(|step| if forward { (index + step) % count } else { (index + count - step) % count })
            .map(|i| &modes[i])
            .find(|mode| mode.quick_switch)
            .or_else(|| modes.get(index))
            .cloned()
            .unwrap_or_else(raw)
    }

    /// entfernt modi mit leerer/doppelter id und gibt jedem modus einen namen.
    pub fn normalize_modes(&mut self) {
        let mut seen = Vec::new();
        self.modes.retain_mut(|mode| {
            if mode.id.trim().is_empty() || seen.contains(&mode.id) {
                return false;
            }
            seen.push(mode.id.clone());
            if mode.name.trim().is_empty() {
                mode.name = mode.id.clone();
            }
            true
        });
        if self.mode(&self.default_mode_id).is_none() {
            self.default_mode_id = RAW_ID.to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn defaults_include_the_presets_and_raw_default() {
        let config = config();
        let modes = config.modes();
        assert_eq!(modes.len(), 6);
        assert_eq!(config.default_mode().id, RAW_ID);
        assert!(modes.iter().any(|mode| mode.id == "email" && mode.refine.is_some()));
    }

    #[test]
    fn hotkey_beats_app_rules() {
        let mut config = config();
        config.modes[3].apps.push("com.apple.mail".into());
        let mode = config.resolve_mode(Some("notes"), Some("com.apple.mail"));
        assert_eq!(mode.id, "notes");
    }

    #[test]
    fn app_rule_selects_the_first_matching_mode() {
        let mut config = config();
        config.modes[1].apps.push("com.example.app".into());
        config.modes[2].apps.push("com.example.app".into());
        let mode = config.resolve_mode(None, Some("com.example.app"));
        assert_eq!(mode.id, "clean");
    }

    #[test]
    fn mail_uses_email_mode_out_of_the_box() {
        let config = config();
        assert_eq!(config.resolve_mode(None, Some("com.apple.mail")).id, "email");
        assert_eq!(config.resolve_mode(None, Some("com.tinyspeck.slackmacgap")).id, "message");
    }

    #[test]
    fn unknown_trigger_and_app_fall_back_to_default() {
        let config = config();
        assert_eq!(config.resolve_mode(Some("nope"), Some("com.example.app")).id, RAW_ID);
    }

    #[test]
    fn cycling_wraps_in_both_directions() {
        let config = config();
        assert_eq!(config.cycle_mode("raw", true).id, "clean");
        assert_eq!(config.cycle_mode("raw", false).id, "prompt");
        assert_eq!(config.cycle_mode("prompt", true).id, "raw");
    }

    #[test]
    fn cycling_skips_modes_without_quick_switch() {
        let mut config = config();
        config.modes[1].quick_switch = false;
        assert_eq!(config.cycle_mode("raw", true).id, "message");
        assert_eq!(config.cycle_mode("message", false).id, "raw");
        // der aktuelle modus muss nicht selbst umschaltbar sein.
        assert_eq!(config.cycle_mode("clean", true).id, "message");
    }

    #[test]
    fn cycling_stays_put_when_nothing_else_is_switchable() {
        let mut config = config();
        for mode in &mut config.modes {
            mode.quick_switch = mode.id == "email";
        }
        assert_eq!(config.cycle_mode("email", true).id, "email");
        assert_eq!(config.cycle_mode("raw", true).id, "email");
    }

    #[test]
    fn backend_round_trips_as_a_string() {
        for backend in [
            Backend::Apple,
            Backend::Local,
            Backend::Endpoint("ollama".into()),
            Backend::Cloud("openai".into()),
        ] {
            let json = serde_json::to_string(&backend).unwrap();
            let parsed: Backend = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, backend);
        }
        assert_eq!(Backend::parse("endpoint:ollama").as_str(), "endpoint:ollama");
    }

    #[test]
    fn empty_refine_prompt_makes_the_mode_raw() {
        let mut mode = preset("clean").unwrap();
        mode.refine.as_mut().unwrap().prompt = "  ".into();
        assert!(mode.is_raw());
    }

    #[test]
    fn normalize_drops_duplicates_and_fixes_the_default() {
        let mut config = config();
        config.modes.push(config.modes[0].clone());
        config.default_mode_id = "missing".into();
        config.normalize_modes();
        assert_eq!(config.modes.len(), 6);
        assert_eq!(config.default_mode_id, RAW_ID);
    }
}
