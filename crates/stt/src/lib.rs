mod apple_speech;
mod parakeet;

use std::path::{Path, PathBuf};

pub use apple_speech::AppleSpeech;
pub use parakeet::Parakeet;

pub const SAMPLE_RATE: u32 = 16_000;

/// welche engine der worker laden soll. die string-form ist stabil, weil sie in der config als `engine.default` gespeichert wird.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineSpec {
    Parakeet { dir: PathBuf },
    AppleSpeech { locale: String },
}

impl EngineSpec {
    pub fn parse(id: &str, models: &Path) -> Self {
        match id.strip_prefix("apple-speech:") {
            Some(locale) if !locale.is_empty() => Self::AppleSpeech { locale: locale.to_owned() },
            _ => Self::Parakeet { dir: models.join(id) },
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Parakeet { dir } => {
                dir.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
            }
            Self::AppleSpeech { locale } => format!("apple-speech:{locale}"),
        }
    }

    pub fn is_parakeet(&self) -> bool {
        matches!(self, Self::Parakeet { .. })
    }
}

#[derive(Default)]
pub struct TranscribeOptions<'a> {
    pub language: Option<&'a str>,
    pub vocabulary: &'a [String],
}

pub trait Transcriber: Send {
    fn transcribe(&mut self, samples: &[f32], options: &TranscribeOptions) -> Result<String, Error>;
}

pub fn load(spec: &EngineSpec) -> Result<Box<dyn Transcriber>, Error> {
    match spec {
        EngineSpec::Parakeet { dir } => Ok(Box::new(Parakeet::load(dir)?)),
        EngineSpec::AppleSpeech { locale } => {
            let locales = tf_apple::speech::locales().map_err(|e| Error::AppleSpeech(e.to_string()))?;
            let normalized = locale.replace('_', "-");
            if !locales.iter().any(|candidate| candidate.id == normalized && candidate.installed) {
                return Err(Error::AppleSpeech(format!("language assets for {locale} are not installed")));
            }
            Ok(Box::new(AppleSpeech::new(locale.clone())))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model file not found: {0}")]
    MissingModel(PathBuf),
    #[error("invalid model: {0}")]
    InvalidModel(String),
    #[error("CoreML: {0}")]
    CoreMl(String),
    #[error("Apple Speech: {0}")]
    AppleSpeech(String),
    #[error("unsupported engine: {0}")]
    Unsupported(String),
}

impl From<tf_coreml::Error> for Error {
    fn from(error: tf_coreml::Error) -> Self {
        match error {
            tf_coreml::Error::MissingModel(path) => Self::MissingModel(path),
            tf_coreml::Error::CoreMl(message) => Self::CoreMl(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_engine_ids() {
        let models = Path::new("/models");
        assert_eq!(EngineSpec::parse("parakeet-tdt-0.6b-v3", models).id(), "parakeet-tdt-0.6b-v3");
        let apple = EngineSpec::parse("apple-speech:de-DE", models);
        assert_eq!(apple.id(), "apple-speech:de-DE");
        assert!(matches!(apple, EngineSpec::AppleSpeech { locale } if locale == "de-DE"));
    }

    #[test]
    fn an_empty_apple_locale_falls_back_to_parakeet() {
        let spec = EngineSpec::parse("apple-speech:", Path::new("/models"));
        assert!(spec.is_parakeet());
    }
}
