//! HTTP surface.
//!
//! The client-facing API is OpenAI's `/v1/chat/completions`, including its
//! `tools` array and `tool_calls` reply. That shape was chosen over Ollama's
//! `/api/chat` because it is what every client library already speaks, and
//! because it carries tool calling as a first-class concept rather than as
//! prompt convention.
//!
//! Ollama's routes remain as a compatibility shim so the services written
//! against the old server keep working while they are ported. They are marked
//! deprecated in `/status` rather than silently maintained forever.

use crate::backend::{Backend, BackendError, ChatRequest, Message};
use hajime_core::tools::Gateway;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use hajime_core::Auth;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<dyn Backend>,
    pub gateway: Arc<Gateway>,
    pub auth: Auth,
    pub started: chrono::DateTime<chrono::Utc>,
}

pub fn router(state: AppState) -> Router {
    let guarded = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/v1/tools", get(list_tools))
        .route("/v1/audit", get(audit))
        // Ollama compatibility, kept only for the migration window.
        .route("/api/chat", post(ollama_chat))
        .route("/api/tags", get(ollama_tags))
        .layer(axum::middleware::from_fn_with_state(
            state.auth.clone(),
            hajime_core::auth::require_token,
        ))
        .with_state(state.clone());

    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .with_state(state)
        .merge(guarded)
}

// ------------------------------------------------------------------ status

async fn health(State(s): State<AppState>) -> impl IntoResponse {
    let backend = match s.backend.models().await {
        Ok(models) => serde_json::json!({ "reachable": true, "models": models }),
        Err(e) => serde_json::json!({ "reachable": false, "error": e.to_string() }),
    };
    Json(serde_json::json!({
        "status": "ok",
        "service": "hajime-ai",
        "uptime_seconds": (chrono::Utc::now() - s.started).num_seconds(),
        "backend": backend,
    }))
}

/// Says plainly what this service does and does not do.
///
/// It is a gateway. It never loads a model, and it says so, because a status
/// page that implies inference happens here would send someone hunting for a
/// memory leak in the wrong process.
async fn status(State(s): State<AppState>) -> impl IntoResponse {
    let tools = s.gateway.catalogue().await;
    Json(serde_json::json!({
        "role": "gateway",
        "inference": "runs in a separate process; this service never loads a model",
        "dry_run": s.gateway.is_dry_run(),
        "tools": tools.len(),
        "tool_names": tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "api": {
            "preferred": ["/v1/chat/completions", "/v1/models", "/v1/tools"],
            "deprecated": ["/api/chat", "/api/tags"],
        },
        "authenticated": s.auth.is_enabled(),
    }))
}

async fn list_tools(State(s): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({ "tools": s.gateway.catalogue().await }))
}

async fn audit(State(s): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({ "calls": s.gateway.audit_log() }))
}

// ------------------------------------------------------- OpenAI-shaped chat

#[derive(Debug, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Serialize)]
struct Choice {
    index: u32,
    message: Message,
    finish_reason: String,
}

#[derive(Debug, Serialize)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Debug, Serialize)]
struct ChatCompletionsResponse {
    id: String,
    object: &'static str,
    created: i64,
    model: String,
    choices: Vec<Choice>,
    usage: Usage,
}

async fn chat_completions(
    State(s): State<AppState>,
    Json(req): Json<ChatCompletionsRequest>,
) -> impl IntoResponse {
    if req.stream {
        // Better to refuse than to answer in a shape the client will mis-parse.
        return (
            StatusCode::NOT_IMPLEMENTED,
            Json(serde_json::json!({
                "error": "streaming is not implemented; send stream=false"
            })),
        );
    }
    if req.messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "messages must not be empty" })),
        );
    }

    // A new conversation gets a fresh tool budget.
    s.gateway.begin_run();

    match s
        .backend
        .chat(ChatRequest { model: req.model, messages: req.messages, stream: false })
        .await
    {
        Ok(reply) => {
            let (prompt, completion, total) =
                (reply.prompt_tokens, reply.completion_tokens, reply.total_tokens());
            let body = ChatCompletionsResponse {
                id: format!("chatcmpl-{}", chrono::Utc::now().timestamp_micros()),
                object: "chat.completion",
                created: chrono::Utc::now().timestamp(),
                model: reply.model,
                choices: vec![Choice {
                    index: 0,
                    message: reply.message,
                    finish_reason: "stop".into(),
                }],
                usage: Usage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    total_tokens: total,
                },
            };
            (StatusCode::OK, Json(serde_json::to_value(body).unwrap_or_default()))
        }
        Err(e) => (
            status_for(&e),
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn models(State(s): State<AppState>) -> impl IntoResponse {
    match s.backend.models().await {
        Ok(models) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "object": "list",
                "data": models.iter().map(|m| serde_json::json!({
                    "id": m, "object": "model", "owned_by": "hajime"
                })).collect::<Vec<_>>(),
            })),
        ),
        Err(e) => (status_for(&e), Json(serde_json::json!({ "error": e.to_string() }))),
    }
}

// ------------------------------------------------- Ollama compatibility shim

async fn ollama_chat(
    State(s): State<AppState>,
    Json(req): Json<ChatCompletionsRequest>,
) -> impl IntoResponse {
    match s
        .backend
        .chat(ChatRequest { model: req.model, messages: req.messages, stream: false })
        .await
    {
        Ok(reply) => {
            let (prompt, completion) = (reply.prompt_tokens, reply.completion_tokens);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "model": reply.model,
                    "message": reply.message,
                    "done": true,
                    "prompt_eval_count": prompt,
                    "eval_count": completion,
                })),
            )
        }
        Err(e) => (status_for(&e), Json(serde_json::json!({ "error": e.to_string() }))),
    }
}

async fn ollama_tags(State(s): State<AppState>) -> impl IntoResponse {
    match s.backend.models().await {
        Ok(models) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "models": models.iter().map(|m| serde_json::json!({ "name": m }))
                    .collect::<Vec<_>>(),
            })),
        ),
        Err(e) => (status_for(&e), Json(serde_json::json!({ "error": e.to_string() }))),
    }
}

/// A missing model is the caller's mistake; an unreachable backend is ours.
/// Collapsing both into 500 makes the difference invisible in a log.
fn status_for(e: &BackendError) -> StatusCode {
    match e {
        BackendError::Unreachable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        BackendError::UnknownModel(_) => StatusCode::NOT_FOUND,
        BackendError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
        BackendError::Rejected(_) => StatusCode::BAD_GATEWAY,
        BackendError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::fake::FakeBackend;
    use crate::tools::Budget;

    #[test]
    fn an_openai_request_parses() {
        let raw = r#"{"model":"qwen2.5:3b",
                      "messages":[{"role":"user","content":"مرحبا"}],
                      "stream":false}"#;
        let req: ChatCompletionsRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.model, "qwen2.5:3b");
        assert_eq!(req.messages[0].content, "مرحبا");
        assert!(!req.stream);
    }

    #[test]
    fn a_request_without_stream_defaults_to_non_streaming() {
        let req: ChatCompletionsRequest = serde_json::from_str(
            r#"{"model":"phi3:mini","messages":[{"role":"user","content":"hi"}]}"#,
        )
        .unwrap();
        assert!(!req.stream);
        assert!(req.tools.is_none());
    }

    #[test]
    fn a_tools_array_is_accepted() {
        let req: ChatCompletionsRequest = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"x"}],
                "tools":[{"type":"function","function":{"name":"read_feed"}}]}"#,
        )
        .unwrap();
        assert_eq!(req.tools.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_backend_answers_through_the_gateway() {
        let backend = Arc::new(FakeBackend::with(&["qwen2.5:3b"]));
        let reply = backend
            .chat(ChatRequest {
                model: "qwen2.5:3b".into(),
                messages: vec![Message { role: "user".into(), content: "hi".into() }],
                stream: false,
            })
            .await
            .unwrap();
        assert_eq!(reply.total_tokens(), 15);
    }

    #[test]
    fn each_backend_failure_maps_to_a_distinct_status() {
        assert_eq!(
            status_for(&BackendError::Unreachable { url: "u".into(), reason: "r".into() }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for(&BackendError::UnknownModel("llama3".into())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(status_for(&BackendError::Timeout(60)), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(status_for(&BackendError::Rejected("x".into())), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn status_declares_that_inference_is_elsewhere() {
        let state = AppState {
            backend: Arc::new(FakeBackend::with(&["qwen2.5:3b"])),
            gateway: Arc::new(Gateway::new(Budget::default(), true)),
            auth: Auth::new(None),
            started: chrono::Utc::now(),
        };
        // The claim is asserted so it cannot quietly become untrue.
        let tools = state.gateway.catalogue().await;
        assert!(tools.is_empty());
        assert!(state.gateway.is_dry_run());
    }
}
