//! WAHA-compatible HTTP surface.
//!
//! The routes and payload shapes match what the production workflows already
//! send, so `hajime-wa` can replace WAHA without editing a single workflow:
//!
//!   POST /api/sendText
//!   X-Api-Key: <key>
//!   {"session": "default", "chatId": "9665…@c.us", "text": "…"}
//!
//! The behaviour that is deliberately *not* inherited is the placeholder this
//! crate used to have, which returned `success: true` and a fabricated message
//! id without contacting WhatsApp at all. A caller that receives an id here has
//! had a message accepted by the bridge.

use crate::bridge::{Bridge, BridgeError};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use hajime_core::history::{History, Record};
use hajime_core::Secret;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub bridge: Arc<dyn Bridge>,
    pub history: Arc<History>,
    /// Callers present this as `X-Api-Key`. WAHA's scheme, kept for
    /// compatibility; the value now comes from the secret store rather than
    /// being written into a workflow file.
    pub api_key: Option<Arc<Secret>>,
    pub started: chrono::DateTime<chrono::Utc>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/sessions/:name", get(session))
        .route("/api/sendText", post(send_text))
        .with_state(state)
}

impl AppState {
    fn authorised(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.api_key else {
            // No key configured. `main` refuses to bind off loopback in that
            // case, so this is a local development posture, not an open door.
            return true;
        };
        let presented = headers
            .get("X-Api-Key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();

        // Constant time: a length-varying comparison leaks the key by timing.
        let a = expected.expose().as_bytes();
        let b = presented.as_bytes();
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn unauthorised() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "X-Api-Key is missing or wrong" })),
    )
}

/// Liveness for this process only.
///
/// It reports the bridge as a separate fact rather than folding it in, because
/// "this service is up" and "WhatsApp is reachable" are different questions and
/// conflating them is how a dead integration looks healthy.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let bridge = match state.bridge.session("default").await {
        Ok(s) => serde_json::json!({
            "reachable": true,
            "status": s.status,
            "connected": s.is_working(),
        }),
        Err(e) => serde_json::json!({
            "reachable": false,
            "connected": false,
            "error": e.to_string(),
        }),
    };
    Json(serde_json::json!({
        "status": "ok",
        "service": "hajime-wa",
        "uptime_seconds": (chrono::Utc::now() - state.started).num_seconds(),
        "bridge": bridge,
    }))
}

async fn session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if !state.authorised(&headers) {
        return unauthorised();
    }
    match state.bridge.session(&name).await {
        Ok(s) => (StatusCode::OK, Json(serde_json::to_value(s).unwrap_or_default())),
        Err(e) => (status_for(&e), Json(serde_json::json!({ "error": e.to_string() }))),
    }
}

#[derive(Debug, Deserialize)]
pub struct SendTextRequest {
    #[serde(default = "default_session")]
    pub session: String,
    #[serde(rename = "chatId")]
    pub chat_id: String,
    pub text: String,
}

fn default_session() -> String {
    "default".to_string()
}

async fn send_text(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SendTextRequest>,
) -> impl IntoResponse {
    if !state.authorised(&headers) {
        return unauthorised();
    }

    if req.chat_id.trim().is_empty() || req.text.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "chatId and text are both required" })),
        );
    }

    let started = chrono::Utc::now();
    match state.bridge.send_text(&req.session, &req.chat_id, &req.text).await {
        Ok(sent) => {
            state.history.append(&Record::success(
                &req.session,
                "sendText",
                &req.chat_id,
                started,
                (chrono::Utc::now() - started).num_milliseconds().max(0) as u64,
                1,
            ));
            (
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "id": sent.id,
                    "chatId": sent.chat_id,
                    "session": req.session,
                })),
            )
        }
        Err(e) => {
            // A failed send is recorded too. A message that did not go out is
            // exactly the event an operator needs to find later.
            state.history.append(&Record::failure(
                &req.session,
                "sendText",
                &req.chat_id,
                started,
                (chrono::Utc::now() - started).num_milliseconds().max(0) as u64,
                1,
                Some("bridge".into()),
                Some(e.to_string()),
            ));
            (status_for(&e), Json(serde_json::json!({ "error": e.to_string() })))
        }
    }
}

/// Distinguish "the bridge is down" from "the session is not paired". The
/// first is an outage, the second needs a human with a phone.
fn status_for(e: &BridgeError) -> StatusCode {
    match e {
        BridgeError::Unreachable { .. } => StatusCode::SERVICE_UNAVAILABLE,
        BridgeError::NotConnected { .. } => StatusCode::CONFLICT,
        BridgeError::Rejected(_) => StatusCode::BAD_GATEWAY,
        BridgeError::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::fake::FakeBridge;

    fn state(bridge: Arc<dyn Bridge>, key: Option<&str>) -> AppState {
        AppState {
            bridge,
            history: Arc::new(History::disabled()),
            api_key: key.map(|k| Arc::new(Secret::new(k))),
            started: chrono::Utc::now(),
        }
    }

    fn headers(key: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(k) = key {
            h.insert("X-Api-Key", k.parse().unwrap());
        }
        h
    }

    #[test]
    fn the_configured_key_is_required_when_set() {
        let s = state(Arc::new(FakeBridge::working()), Some("s3cret"));
        assert!(s.authorised(&headers(Some("s3cret"))));
        assert!(!s.authorised(&headers(Some("wrong"))));
        assert!(!s.authorised(&headers(Some("s3cre"))));
        assert!(!s.authorised(&headers(None)));
    }

    #[test]
    fn with_no_key_configured_requests_pass() {
        let s = state(Arc::new(FakeBridge::working()), None);
        assert!(s.authorised(&headers(None)));
    }

    #[test]
    fn the_request_shape_matches_what_the_workflows_send() {
        // Taken from the WAHA Auto-Reply workflow verbatim.
        let raw = r#"{"session":"default","chatId":"966504211844@c.us","text":"مرحباً"}"#;
        let req: SendTextRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.session, "default");
        assert_eq!(req.chat_id, "966504211844@c.us");
        assert_eq!(req.text, "مرحباً");
    }

    #[test]
    fn a_missing_session_defaults_rather_than_failing() {
        let req: SendTextRequest =
            serde_json::from_str(r#"{"chatId":"9665@c.us","text":"hi"}"#).unwrap();
        assert_eq!(req.session, "default");
    }

    #[tokio::test]
    async fn a_send_reaches_the_bridge() {
        let bridge = Arc::new(FakeBridge::working());
        let sent = bridge.send_text("default", "9665@c.us", "hello").await.unwrap();
        assert!(sent.id.starts_with("wamid."));
        assert_eq!(bridge.sent_messages()[0].2, "hello");
    }

    #[tokio::test]
    async fn an_unpaired_session_yields_conflict_not_success() {
        // This is the state the live WAHA session is in right now.
        let bridge = FakeBridge::with_status("SCAN_QR_CODE");
        let err = bridge.send_text("default", "9665@c.us", "hi").await.unwrap_err();
        assert_eq!(status_for(&err), StatusCode::CONFLICT);
        assert!(bridge.sent_messages().is_empty());
    }

    #[tokio::test]
    async fn a_dead_bridge_yields_service_unavailable() {
        let bridge = FakeBridge::down();
        let err = bridge.send_text("default", "9665@c.us", "hi").await.unwrap_err();
        assert_eq!(status_for(&err), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn every_bridge_failure_maps_to_a_distinct_status() {
        assert_eq!(
            status_for(&BridgeError::Unreachable { url: "u".into(), reason: "r".into() }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for(&BridgeError::NotConnected {
                session: "default".into(),
                state: "SCAN_QR_CODE".into()
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(status_for(&BridgeError::Rejected("x".into())), StatusCode::BAD_GATEWAY);
    }
}
