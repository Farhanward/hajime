//! The link to WhatsApp.
//!
//! The protocol itself is spoken by `whatsmeow`, a Go library, in a separate
//! process. Rust talks to it over loopback HTTP.
//!
//! Two processes rather than one binary, deliberately:
//!
//! - `whatsmeow` is the only maintained implementation of WhatsApp's
//!   multi-device protocol outside the official clients. Reimplementing it in
//!   Rust is months of reverse engineering for no functional gain.
//! - The session holds long-lived device keys. Keeping it in its own process
//!   means a crash or restart of the API does not drop the WhatsApp session,
//!   and re-pairing by QR is not needed.
//!
//! The trait exists so the API can be tested without a WhatsApp account.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("the WhatsApp bridge is not reachable at {url}: {reason}")]
    Unreachable { url: String, reason: String },
    #[error("session '{session}' is not connected: {state}")]
    NotConnected { session: String, state: String },
    #[error("the bridge rejected the request: {0}")]
    Rejected(String),
    #[error("{0}")]
    Other(String),
}

/// What the bridge reports about a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionState {
    pub name: String,
    /// `WORKING`, `SCAN_QR_CODE`, `STARTING`, `FAILED`, `STOPPED`.
    /// Mirrors WAHA's vocabulary so existing dashboards keep reading it.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
}

impl SessionState {
    pub fn is_working(&self) -> bool {
        self.status.eq_ignore_ascii_case("WORKING")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SentMessage {
    pub id: String,
    pub chat_id: String,
}

#[async_trait]
pub trait Bridge: Send + Sync {
    async fn session(&self, name: &str) -> Result<SessionState, BridgeError>;
    async fn send_text(
        &self,
        session: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<SentMessage, BridgeError>;
}

/// Talks to the `whatsmeow` bridge over loopback HTTP.
pub struct HttpBridge {
    base_url: String,
    client: reqwest::Client,
}

impl HttpBridge {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("a default reqwest client is always valid"),
        }
    }

    pub fn url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl Bridge for HttpBridge {
    async fn session(&self, name: &str) -> Result<SessionState, BridgeError> {
        let url = format!("{}/session/{name}", self.base_url);
        let response = self.client.get(&url).send().await.map_err(|e| {
            BridgeError::Unreachable { url: url.clone(), reason: e.to_string() }
        })?;

        if !response.status().is_success() {
            return Err(BridgeError::Rejected(format!(
                "session lookup returned HTTP {}",
                response.status().as_u16()
            )));
        }
        response
            .json()
            .await
            .map_err(|e| BridgeError::Other(format!("unreadable session response: {e}")))
    }

    async fn send_text(
        &self,
        session: &str,
        chat_id: &str,
        text: &str,
    ) -> Result<SentMessage, BridgeError> {
        // Refuse before sending rather than discovering afterwards. A caller
        // that gets an id back is entitled to believe the message left.
        let state = self.session(session).await?;
        if !state.is_working() {
            return Err(BridgeError::NotConnected {
                session: session.to_string(),
                state: state.status,
            });
        }

        let url = format!("{}/send/text", self.base_url);
        let response = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "session": session,
                "chatId": chat_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|e| BridgeError::Unreachable { url, reason: e.to_string() })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(BridgeError::Rejected(format!(
                "HTTP {}: {}",
                status.as_u16(),
                body.chars().take(200).collect::<String>()
            )));
        }

        serde_json::from_str(&body)
            .map_err(|e| BridgeError::Other(format!("unreadable send response: {e}")))
    }
}

#[cfg(test)]
pub mod fake {
    //! A bridge for tests. Records what it was asked to send so a test can
    //! assert on it, and can be told to fail.

    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct FakeBridge {
        pub status: Mutex<String>,
        pub sent: Mutex<Vec<(String, String, String)>>,
        pub unreachable: Mutex<bool>,
    }

    impl FakeBridge {
        pub fn working() -> Self {
            Self {
                status: Mutex::new("WORKING".into()),
                ..Default::default()
            }
        }

        pub fn with_status(status: &str) -> Self {
            Self {
                status: Mutex::new(status.into()),
                ..Default::default()
            }
        }

        pub fn down() -> Self {
            Self {
                status: Mutex::new("STOPPED".into()),
                unreachable: Mutex::new(true),
                ..Default::default()
            }
        }

        pub fn sent_messages(&self) -> Vec<(String, String, String)> {
            self.sent.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Bridge for FakeBridge {
        async fn session(&self, name: &str) -> Result<SessionState, BridgeError> {
            if *self.unreachable.lock().unwrap() {
                return Err(BridgeError::Unreachable {
                    url: "http://127.0.0.1:3001".into(),
                    reason: "connection refused".into(),
                });
            }
            Ok(SessionState {
                name: name.to_string(),
                status: self.status.lock().unwrap().clone(),
                phone: Some("+9665XXXXXXXX".into()),
            })
        }

        async fn send_text(
            &self,
            session: &str,
            chat_id: &str,
            text: &str,
        ) -> Result<SentMessage, BridgeError> {
            let state = self.session(session).await?;
            if !state.is_working() {
                return Err(BridgeError::NotConnected {
                    session: session.to_string(),
                    state: state.status,
                });
            }
            self.sent.lock().unwrap().push((
                session.to_string(),
                chat_id.to_string(),
                text.to_string(),
            ));
            Ok(SentMessage {
                id: format!("wamid.TEST{}", self.sent.lock().unwrap().len()),
                chat_id: chat_id.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBridge;
    use super::*;

    #[tokio::test]
    async fn a_working_session_sends() {
        let b = FakeBridge::working();
        let sent = b.send_text("default", "966500000000@c.us", "hi").await.unwrap();
        assert_eq!(sent.chat_id, "966500000000@c.us");
        assert_eq!(b.sent_messages().len(), 1);
    }

    #[tokio::test]
    async fn a_session_awaiting_a_qr_scan_refuses_to_send() {
        // The state the production WAHA session is actually in today.
        let b = FakeBridge::with_status("SCAN_QR_CODE");
        let err = b.send_text("default", "966500000000@c.us", "hi").await.unwrap_err();
        assert!(matches!(err, BridgeError::NotConnected { .. }));
        assert!(b.sent_messages().is_empty(), "nothing may be recorded as sent");
        assert!(err.to_string().contains("SCAN_QR_CODE"), "got: {err}");
    }

    #[tokio::test]
    async fn an_unreachable_bridge_is_an_error_not_a_silent_success() {
        let b = FakeBridge::down();
        let err = b.send_text("default", "966500000000@c.us", "hi").await.unwrap_err();
        assert!(matches!(err, BridgeError::Unreachable { .. }));
        assert!(b.sent_messages().is_empty());
    }

    #[test]
    fn only_working_counts_as_connected() {
        let working = |s: &str| SessionState {
            name: "default".into(),
            status: s.into(),
            phone: None,
        };
        assert!(working("WORKING").is_working());
        assert!(working("working").is_working());
        for other in ["SCAN_QR_CODE", "STARTING", "FAILED", "STOPPED", ""] {
            assert!(!working(other).is_working(), "{other} must not count");
        }
    }

    #[test]
    fn the_base_url_is_normalised() {
        assert_eq!(HttpBridge::new("http://127.0.0.1:3001/").url(), "http://127.0.0.1:3001");
    }
}
