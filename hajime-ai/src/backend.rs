//! The inference backend.
//!
//! Runs in its own process, always. Embedding the model in this binary would
//! mean every restart of the API reloads gigabytes of weights, an out-of-memory
//! kill in inference takes the API down with it, and swapping engines needs a
//! rebuild. A separate process makes the API restart instantly and keeps a
//! crash contained.
//!
//! The backend is reached over HTTP. `llama.cpp`'s server and Ollama both speak
//! a compatible enough dialect that one client covers either, which is what
//! lets the engine be replaced without touching this crate.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("the inference backend is not reachable at {url}: {reason}")]
    Unreachable { url: String, reason: String },
    #[error("model '{0}' is not loaded on the backend")]
    UnknownModel(String),
    #[error("the backend rejected the request: {0}")]
    Rejected(String),
    #[error("the request exceeded {0} seconds")]
    Timeout(u64),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatReply {
    pub model: String,
    pub message: Message,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
}

impl ChatReply {
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens + self.completion_tokens
    }
}

#[async_trait]
pub trait Backend: Send + Sync {
    /// Models the backend can serve right now.
    async fn models(&self) -> Result<Vec<String>, BackendError>;
    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, BackendError>;
}

pub struct HttpBackend {
    base_url: String,
    client: reqwest::Client,
    timeout_secs: u64,
}

impl HttpBackend {
    pub fn new(base_url: impl Into<String>, timeout_secs: u64) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(timeout_secs))
                .build()
                .expect("a default reqwest client is always valid"),
            timeout_secs,
        }
    }

    pub fn url(&self) -> &str {
        &self.base_url
    }
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaTag>,
}

#[derive(Deserialize)]
struct OllamaTag {
    name: String,
}

#[derive(Deserialize)]
struct OllamaChatReply {
    #[serde(default)]
    model: String,
    message: Message,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    eval_count: u32,
}

#[async_trait]
impl Backend for HttpBackend {
    async fn models(&self) -> Result<Vec<String>, BackendError> {
        let url = format!("{}/api/tags", self.base_url);
        let response = self.client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                BackendError::Timeout(self.timeout_secs)
            } else {
                BackendError::Unreachable { url: url.clone(), reason: e.to_string() }
            }
        })?;

        if !response.status().is_success() {
            return Err(BackendError::Rejected(format!(
                "model list returned HTTP {}",
                response.status().as_u16()
            )));
        }
        let tags: OllamaTags = response
            .json()
            .await
            .map_err(|e| BackendError::Other(format!("unreadable model list: {e}")))?;
        Ok(tags.models.into_iter().map(|m| m.name).collect())
    }

    async fn chat(&self, req: ChatRequest) -> Result<ChatReply, BackendError> {
        let url = format!("{}/api/chat", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "model": req.model,
                "messages": req.messages,
                // Streaming is not forwarded yet; a partial answer needs a
                // different contract on every route that returns one.
                "stream": false,
            }))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    BackendError::Timeout(self.timeout_secs)
                } else {
                    BackendError::Unreachable { url: url.clone(), reason: e.to_string() }
                }
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status == reqwest::StatusCode::NOT_FOUND || body.contains("not found") {
            return Err(BackendError::UnknownModel(req.model));
        }
        if !status.is_success() {
            return Err(BackendError::Rejected(format!(
                "HTTP {}: {}",
                status.as_u16(),
                body.chars().take(200).collect::<String>()
            )));
        }

        let reply: OllamaChatReply = serde_json::from_str(&body)
            .map_err(|e| BackendError::Other(format!("unreadable reply: {e}")))?;

        Ok(ChatReply {
            model: if reply.model.is_empty() { req.model } else { reply.model },
            message: reply.message,
            prompt_tokens: reply.prompt_eval_count,
            completion_tokens: reply.eval_count,
        })
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeBackend {
        pub models: Vec<String>,
        pub calls: Mutex<Vec<ChatRequest>>,
        pub down: bool,
    }

    impl FakeBackend {
        pub fn with(models: &[&str]) -> Self {
            Self {
                models: models.iter().map(|m| m.to_string()).collect(),
                calls: Mutex::new(Vec::new()),
                down: false,
            }
        }

        pub fn down() -> Self {
            Self { models: Vec::new(), calls: Mutex::new(Vec::new()), down: true }
        }

        pub fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl Backend for FakeBackend {
        async fn models(&self) -> Result<Vec<String>, BackendError> {
            if self.down {
                return Err(BackendError::Unreachable {
                    url: "http://127.0.0.1:11435".into(),
                    reason: "connection refused".into(),
                });
            }
            Ok(self.models.clone())
        }

        async fn chat(&self, req: ChatRequest) -> Result<ChatReply, BackendError> {
            if self.down {
                return Err(BackendError::Unreachable {
                    url: "http://127.0.0.1:11435".into(),
                    reason: "connection refused".into(),
                });
            }
            if !self.models.contains(&req.model) {
                return Err(BackendError::UnknownModel(req.model));
            }
            self.calls.lock().unwrap().push(req.clone());
            Ok(ChatReply {
                model: req.model,
                message: Message { role: "assistant".into(), content: "ok".into() },
                prompt_tokens: 10,
                completion_tokens: 5,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBackend;
    use super::*;

    fn req(model: &str) -> ChatRequest {
        ChatRequest {
            model: model.into(),
            messages: vec![Message { role: "user".into(), content: "hi".into() }],
            stream: false,
        }
    }

    #[tokio::test]
    async fn a_loaded_model_answers() {
        // The two models actually pulled on the server.
        let b = FakeBackend::with(&["qwen2.5:3b", "phi3:mini"]);
        let reply = b.chat(req("qwen2.5:3b")).await.unwrap();
        assert_eq!(reply.message.role, "assistant");
        assert_eq!(reply.total_tokens(), 15);
        assert_eq!(b.call_count(), 1);
    }

    #[tokio::test]
    async fn an_unloaded_model_is_named_in_the_error() {
        let b = FakeBackend::with(&["qwen2.5:3b"]);
        let err = b.chat(req("llama3")).await.unwrap_err();
        assert!(matches!(err, BackendError::UnknownModel(ref m) if m == "llama3"));
        assert_eq!(b.call_count(), 0, "nothing may be recorded as run");
    }

    #[tokio::test]
    async fn a_dead_backend_is_an_error_not_an_empty_answer() {
        let b = FakeBackend::down();
        assert!(matches!(
            b.chat(req("qwen2.5:3b")).await.unwrap_err(),
            BackendError::Unreachable { .. }
        ));
        assert!(b.models().await.is_err());
    }

    #[test]
    fn token_counts_add_up() {
        let r = ChatReply {
            model: "qwen2.5:3b".into(),
            message: Message { role: "assistant".into(), content: "x".into() },
            prompt_tokens: 120,
            completion_tokens: 45,
        };
        assert_eq!(r.total_tokens(), 165);
    }

    #[test]
    fn the_base_url_is_normalised() {
        assert_eq!(HttpBackend::new("http://127.0.0.1:11435/", 60).url(), "http://127.0.0.1:11435");
    }

    #[test]
    fn an_ollama_reply_parses_into_our_shape() {
        // Shaped exactly as Ollama's /api/chat returns.
        let raw = r#"{"model":"qwen2.5:3b","message":{"role":"assistant","content":"مرحباً"},
                      "prompt_eval_count":31,"eval_count":12,"done":true}"#;
        let parsed: OllamaChatReply = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.message.content, "مرحباً");
        assert_eq!(parsed.prompt_eval_count, 31);
        assert_eq!(parsed.eval_count, 12);
    }
}
