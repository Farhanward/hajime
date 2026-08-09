//! Bearer-token authentication for the control endpoints.
//!
//! Webhook paths are deliberately left open: they are the public surface, and
//! their secrecy comes from the path itself, exactly as in n8n. Everything that
//! lists workflows or starts a run requires a token, because those endpoints
//! can reach every system the workflows touch.
//!
//! With no token configured the service refuses to bind to anything other than
//! loopback. That decision lives in `main.rs`; this module only checks.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Auth {
    token: Option<Arc<String>>,
}

impl Auth {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: token
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(Arc::new),
        }
    }

    pub fn from_env() -> Self {
        Self::new(std::env::var("HAJIME_TOKEN").ok())
    }

    pub fn is_enabled(&self) -> bool {
        self.token.is_some()
    }

    /// Compare in constant time so a wrong token leaks nothing through timing.
    pub fn accepts(&self, presented: &str) -> bool {
        let Some(expected) = &self.token else {
            // No token configured: the caller is responsible for staying on
            // loopback. Returning true here keeps local development usable.
            return true;
        };
        let a = expected.as_bytes();
        let b = presented.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

/// Extract a bearer token from an `Authorization` header.
fn bearer(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

pub async fn require_token(
    State(auth): State<Auth>,
    request: Request,
    next: Next,
) -> Response {
    if !auth.is_enabled() {
        return next.run(request).await;
    }
    match bearer(&request) {
        Some(token) if auth.accepts(token) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "a bearer token is required for this endpoint"
            })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_token_configured_accepts_anything() {
        let auth = Auth::new(None);
        assert!(!auth.is_enabled());
        assert!(auth.accepts(""));
        assert!(auth.accepts("whatever"));
    }

    #[test]
    fn a_blank_token_counts_as_unconfigured() {
        assert!(!Auth::new(Some("   ".into())).is_enabled());
        assert!(!Auth::new(Some(String::new())).is_enabled());
    }

    #[test]
    fn the_right_token_is_accepted_and_others_are_not() {
        let auth = Auth::new(Some("s3cret".into()));
        assert!(auth.is_enabled());
        assert!(auth.accepts("s3cret"));
        assert!(!auth.accepts("s3cres"));
        assert!(!auth.accepts("s3cret "));
        assert!(!auth.accepts(""));
        assert!(!auth.accepts("s3cretlonger"));
    }

    #[test]
    fn surrounding_whitespace_in_config_is_ignored() {
        let auth = Auth::new(Some("  padded  ".into()));
        assert!(auth.accepts("padded"));
    }
}
