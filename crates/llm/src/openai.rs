use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Error, Refiner, Request};

/// jeder openai-kompatible `/chat/completions`-server: ollama, lm studio, mlx, vllm, llama-server und die gehosteten anbieter.
pub struct OpenAiRefiner {
    base_url: String,
    model: String,
    api_key: Option<String>,
    /// wird als `chat_template_kwargs` gesendet; schaltet das thinking von qwen3 ab.
    disable_thinking: bool,
}

impl OpenAiRefiner {
    pub fn new(base_url: &str, model: &str, api_key: Option<String>) -> Result<Self, Error> {
        let base_url = base_url.trim().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(Error::Unavailable("endpoint has no base URL".into()));
        }
        if model.trim().is_empty() {
            return Err(Error::Unavailable("endpoint has no model".into()));
        }
        Ok(Self { base_url, model: model.trim().to_owned(), api_key, disable_thinking: false })
    }

    pub fn without_thinking(mut self) -> Self {
        self.disable_thinking = true;
        self
    }

    fn body(&self, request: &Request) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": request.system },
                { "role": "user", "content": request.user },
            ],
            "temperature": request.temperature,
            "max_tokens": request.max_tokens,
            "stream": false,
        });
        if self.disable_thinking {
            body["chat_template_kwargs"] = json!({ "enable_thinking": false });
        }
        body
    }
}

impl Refiner for OpenAiRefiner {
    fn refine(&mut self, request: &Request) -> Result<String, Error> {
        let agent = agent(request.deadline)?;
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.body(request).to_string();
        let mut call = agent.post(&url).set("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            call = call.set("Authorization", &format!("Bearer {key}"));
        }
        let response = match call.send_string(&body) {
            Ok(response) => response,
            Err(error) => return Err(map_transport(error, request.deadline)),
        };
        let text = response.into_string().map_err(|e| Error::Io(e.to_string()))?;
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Io(format!("invalid JSON from the endpoint: {e}")))?;
        extract_text(&parsed).ok_or_else(|| Error::Io("the endpoint returned no message".into()))
    }
}

fn extract_text(value: &Value) -> Option<String> {
    let content = &value["choices"][0]["message"]["content"];
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    // manche server liefern ein array aus content-parts.
    content.as_array()?.iter().find_map(|part| part["text"].as_str().map(str::to_owned))
}

pub fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>, Error> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err(Error::Unavailable("endpoint has no base URL".into()));
    }
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(10))
        .user_agent("tf/0.1")
        .build();
    let mut call = agent.get(&format!("{base_url}/models"));
    if let Some(key) = api_key {
        call = call.set("Authorization", &format!("Bearer {key}"));
    }
    let response = call.call().map_err(|error| match error {
        ureq::Error::Status(status, response) => Error::Http { status, body: read_body(response) },
        ureq::Error::Transport(transport) => Error::Io(transport.to_string()),
    })?;
    let text = response.into_string().map_err(|e| Error::Io(e.to_string()))?;
    let parsed: Value =
        serde_json::from_str(&text).map_err(|e| Error::Io(format!("invalid JSON from the endpoint: {e}")))?;
    let mut models: Vec<String> = parsed["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str().map(str::to_owned))
        .collect();
    models.sort();
    Ok(models)
}

fn agent(deadline: Instant) -> Result<ureq::Agent, Error> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::Timeout);
    }
    Ok(ureq::AgentBuilder::new()
        .timeout_connect(remaining.min(Duration::from_secs(5)))
        .timeout_read(remaining)
        .timeout_write(remaining)
        .user_agent("tf/0.1")
        .build())
}

fn map_transport(error: ureq::Error, deadline: Instant) -> Error {
    match error {
        ureq::Error::Status(status, response) => Error::Http { status, body: read_body(response) },
        ureq::Error::Transport(transport) => {
            if Instant::now() >= deadline
                || matches!(transport.kind(), ureq::ErrorKind::Io)
                    && transport.to_string().to_lowercase().contains("timed out")
            {
                Error::Timeout
            } else {
                Error::Io(transport.to_string())
            }
        }
    }
}

fn read_body(response: ureq::Response) -> String {
    response.into_string().map(|body| body.chars().take(2048).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_disables_thinking_only_when_requested() {
        let request = Request::new("sys", "user", 0.2, 300, Duration::from_secs(1));
        let plain = OpenAiRefiner::new("http://localhost:1/v1", "m", None).unwrap();
        assert!(plain.body(&request)["chat_template_kwargs"].is_null());
        let local = plain.without_thinking();
        assert_eq!(local.body(&request)["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn empty_text_is_an_error() {
        let value = json!({ "choices": [{ "message": { "content": "" } }] });
        assert_eq!(extract_text(&value).as_deref(), Some(""));
        assert!(extract_text(&json!({})).is_none());
    }

    #[test]
    fn content_parts_are_supported() {
        let value = json!({ "choices": [{ "message": { "content": [{ "type": "text", "text": "hi" }] } }] });
        assert_eq!(extract_text(&value).as_deref(), Some("hi"));
    }

    #[test]
    fn empty_configuration_is_refused() {
        assert!(OpenAiRefiner::new("", "m", None).is_err());
        assert!(OpenAiRefiner::new("http://x/v1", " ", None).is_err());
    }
}
