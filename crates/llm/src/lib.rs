//! alles hier blockiert; der pipeline-worker besitzt die refiner und ruft sie aus seinem eigenen thread. kein streaming: eine aufnahme wird in einer anfrage verarbeitet.

mod apple;
mod local;
mod openai;

use std::path::PathBuf;
use std::time::{Duration, Instant};

pub use apple::AppleRefiner;
pub use local::{LocalConfig, LocalRefiner};
pub use openai::OpenAiRefiner;

/// räumt einen llama-server auf, der nach einem crash übrig blieb. einmal beim start aufrufen.
pub fn cleanup_local(state_dir: &std::path::Path) {
    local::cleanup(state_dir);
}

#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub temperature: f32,
    pub max_tokens: u32,
    pub deadline: Instant,
}

impl Request<'_> {
    pub fn new<'a>(
        system: &'a str,
        user: &'a str,
        temperature: f32,
        max_tokens: u32,
        timeout: Duration,
    ) -> Request<'a> {
        Request { system, user, temperature, max_tokens, deadline: Instant::now() + timeout }
    }
}

pub trait Refiner: Send {
    fn refine(&mut self, request: &Request) -> Result<String, Error>;
    /// bereitet das backend ohne request vor. wird beim start der aufnahme in einem modus aufgerufen, der dieses backend nutzt, damit ein lokaler server laden kann, während der nutzer spricht.
    fn warm(&mut self) {}
    fn tick(&mut self) {}
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Unavailable(String),
    #[error("timed out")]
    Timeout,
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Io(String),
}

impl Error {
    /// kurzer, anzeigbarer grund für den roh-text-fallback-hinweis.
    pub fn reason(&self) -> String {
        match self {
            Self::Unavailable(reason) => format!("LLM unavailable ({reason})"),
            Self::Timeout => "LLM timed out".to_owned(),
            Self::Http { status, .. } => format!("LLM error {status}"),
            Self::Refused(_) => "LLM refused the request".to_owned(),
            Self::Io(reason) => format!("LLM failed ({reason})"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

/// alles, was `build` braucht, ohne abhängigkeit von der config-crate.
#[derive(Clone, Debug)]
pub enum Spec {
    Apple,
    Endpoint(Endpoint),
    Local { config: LocalConfig, model: PathBuf },
}

pub fn build(spec: Spec) -> Result<Box<dyn Refiner>, Error> {
    match spec {
        Spec::Apple => Ok(Box::new(AppleRefiner::new())),
        Spec::Endpoint(endpoint) => {
            Ok(Box::new(OpenAiRefiner::new(&endpoint.base_url, &endpoint.model, endpoint.api_key)?))
        }
        Spec::Local { config, model } => Ok(Box::new(LocalRefiner::new(config, model)?)),
    }
}

/// holt die modellnamen, die ein openai-kompatibler endpoint anbietet. vom test-knopf in den einstellungen genutzt.
pub fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, Error> {
    openai::list_models(base_url, api_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_sets_a_deadline() {
        let request = Request::new("system", "user", 0.2, 300, Duration::from_millis(500));
        assert!(request.deadline > Instant::now());
        assert!(request.deadline <= Instant::now() + Duration::from_millis(600));
    }

    #[test]
    fn error_reasons_are_short() {
        assert_eq!(Error::Timeout.reason(), "LLM timed out");
        assert!(Error::Http { status: 500, body: "x".into() }.reason().contains("500"));
    }
}
