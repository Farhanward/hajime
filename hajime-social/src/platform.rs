//! One client per social platform.
//!
//! Each platform gets its own implementation because their APIs agree on
//! nothing: the auth header, the body shape, the success response and the
//! error format all differ. A single "post" abstraction over them would be a
//! lie that breaks on the first error.
//!
//! Credentials come from the secret store, never from a workflow file. They
//! were extracted from the Postiz database before it was stopped.

use async_trait::async_trait;
use hajime_core::Secret;
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum PostError {
    #[error("{platform} is not configured: missing secret '{secret}'")]
    NotConfigured { platform: &'static str, secret: String },
    #[error("{platform} rejected the post: {reason}")]
    Rejected { platform: &'static str, reason: String },
    #[error("could not reach {platform}: {reason}")]
    Unreachable { platform: &'static str, reason: String },
    #[error("{platform} credentials have expired: {detail}")]
    Expired { platform: &'static str, detail: String },
    #[error("the text is {len} characters; {platform} allows {limit}")]
    TooLong { platform: &'static str, len: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Posted {
    pub platform: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[async_trait]
pub trait Platform: Send + Sync {
    fn name(&self) -> &'static str;
    /// Longest post the platform accepts, in characters.
    fn limit(&self) -> usize;
    async fn post(&self, text: &str) -> Result<Posted, PostError>;

    /// Reject before spending a network call, and count characters rather
    /// than bytes: Arabic text is multi-byte, so a byte count would refuse
    /// posts that are well within the limit.
    fn check_length(&self, text: &str) -> Result<(), PostError> {
        let len = text.chars().count();
        if len > self.limit() {
            return Err(PostError::TooLong {
                platform: self.name(),
                len,
                limit: self.limit(),
            });
        }
        Ok(())
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("hajime-social/1.0")
        .build()
        .expect("a default reqwest client is always valid")
}

// ------------------------------------------------------------------ Mastodon

pub struct Mastodon {
    instance: String,
    token: Secret,
    http: reqwest::Client,
}

impl Mastodon {
    pub fn new(instance: impl Into<String>, token: Secret) -> Self {
        Self {
            instance: instance.into().trim_end_matches('/').to_string(),
            token,
            http: client(),
        }
    }
}

#[async_trait]
impl Platform for Mastodon {
    fn name(&self) -> &'static str {
        "mastodon"
    }
    fn limit(&self) -> usize {
        500
    }

    async fn post(&self, text: &str) -> Result<Posted, PostError> {
        self.check_length(text)?;
        let response = self
            .http
            .post(format!("{}/api/v1/statuses", self.instance))
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({ "status": text }))
            .send()
            .await
            .map_err(|e| PostError::Unreachable {
                platform: "mastodon",
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PostError::Expired {
                platform: "mastodon",
                detail: "the access token was rejected".into(),
            });
        }
        if !status.is_success() {
            return Err(PostError::Rejected {
                platform: "mastodon",
                reason: format!("HTTP {}: {}", status.as_u16(), clip(&body)),
            });
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(Posted {
            platform: "mastodon".into(),
            id: parsed["id"].as_str().unwrap_or_default().to_string(),
            url: parsed["url"].as_str().map(str::to_string),
        })
    }
}

// ----------------------------------------------------------------------- X

pub struct X {
    token: Secret,
    http: reqwest::Client,
}

impl X {
    pub fn new(token: Secret) -> Self {
        Self { token, http: client() }
    }
}

#[async_trait]
impl Platform for X {
    fn name(&self) -> &'static str {
        "x"
    }
    fn limit(&self) -> usize {
        280
    }

    async fn post(&self, text: &str) -> Result<Posted, PostError> {
        self.check_length(text)?;
        let response = self
            .http
            .post("https://api.twitter.com/2/tweets")
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .map_err(|e| PostError::Unreachable { platform: "x", reason: e.to_string() })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PostError::Expired {
                platform: "x",
                detail: "the access token was rejected".into(),
            });
        }
        if !status.is_success() {
            return Err(PostError::Rejected {
                platform: "x",
                reason: format!("HTTP {}: {}", status.as_u16(), clip(&body)),
            });
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        let id = parsed["data"]["id"].as_str().unwrap_or_default().to_string();
        Ok(Posted {
            url: (!id.is_empty()).then(|| format!("https://x.com/i/status/{id}")),
            platform: "x".into(),
            id,
        })
    }
}

// ------------------------------------------------------------------ Bluesky

pub struct Bluesky {
    handle: String,
    /// AT Protocol calls this the accessJwt.
    token: Secret,
    http: reqwest::Client,
}

impl Bluesky {
    pub fn new(handle: impl Into<String>, token: Secret) -> Self {
        Self { handle: handle.into(), token, http: client() }
    }
}

#[async_trait]
impl Platform for Bluesky {
    fn name(&self) -> &'static str {
        "bluesky"
    }
    fn limit(&self) -> usize {
        300
    }

    async fn post(&self, text: &str) -> Result<Posted, PostError> {
        self.check_length(text)?;
        let response = self
            .http
            .post("https://bsky.social/xrpc/com.atproto.repo.createRecord")
            .bearer_auth(self.token.expose())
            .json(&serde_json::json!({
                "repo": self.handle,
                "collection": "app.bsky.feed.post",
                "record": {
                    "$type": "app.bsky.feed.post",
                    "text": text,
                    "createdAt": chrono::Utc::now().to_rfc3339(),
                }
            }))
            .send()
            .await
            .map_err(|e| PostError::Unreachable {
                platform: "bluesky",
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        // Bluesky's access token is short-lived by design; the refresh token
        // held alongside it is what renews the session.
        if status == reqwest::StatusCode::BAD_REQUEST && body.contains("ExpiredToken") {
            return Err(PostError::Expired {
                platform: "bluesky",
                detail: "accessJwt expired; refresh the session".into(),
            });
        }
        if !status.is_success() {
            return Err(PostError::Rejected {
                platform: "bluesky",
                reason: format!("HTTP {}: {}", status.as_u16(), clip(&body)),
            });
        }

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(Posted {
            platform: "bluesky".into(),
            id: parsed["uri"].as_str().unwrap_or_default().to_string(),
            url: None,
        })
    }
}

// ----------------------------------------------------------------- LinkedIn

pub struct LinkedIn {
    author_urn: String,
    token: Secret,
    http: reqwest::Client,
}

impl LinkedIn {
    pub fn new(author_urn: impl Into<String>, token: Secret) -> Self {
        Self { author_urn: author_urn.into(), token, http: client() }
    }
}

#[async_trait]
impl Platform for LinkedIn {
    fn name(&self) -> &'static str {
        "linkedin"
    }
    fn limit(&self) -> usize {
        3000
    }

    async fn post(&self, text: &str) -> Result<Posted, PostError> {
        self.check_length(text)?;
        let response = self
            .http
            .post("https://api.linkedin.com/v2/ugcPosts")
            .bearer_auth(self.token.expose())
            .header("X-Restli-Protocol-Version", "2.0.0")
            .json(&serde_json::json!({
                "author": self.author_urn,
                "lifecycleState": "PUBLISHED",
                "specificContent": {
                    "com.linkedin.ugc.ShareContent": {
                        "shareCommentary": { "text": text },
                        "shareMediaCategory": "NONE"
                    }
                },
                "visibility": {
                    "com.linkedin.ugc.MemberNetworkVisibility": "PUBLIC"
                }
            }))
            .send()
            .await
            .map_err(|e| PostError::Unreachable {
                platform: "linkedin",
                reason: e.to_string(),
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // The stored LinkedIn token carries no refresh token and expires
            // on 2026-09-03. When that lands, re-authorisation is the only
            // path, so the message says so instead of suggesting a retry.
            return Err(PostError::Expired {
                platform: "linkedin",
                detail: "no refresh token exists; re-authorise the app".into(),
            });
        }
        if !status.is_success() {
            return Err(PostError::Rejected {
                platform: "linkedin",
                reason: format!("HTTP {}: {}", status.as_u16(), clip(&body)),
            });
        }

        let id = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .unwrap_or_default();
        Ok(Posted { platform: "linkedin".into(), id, url: None })
    }
}

fn clip(body: &str) -> String {
    body.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> Secret {
        Secret::new("test-token")
    }

    #[test]
    fn each_platform_reports_its_own_limit() {
        assert_eq!(X::new(secret()).limit(), 280);
        assert_eq!(Bluesky::new("me.bsky.social", secret()).limit(), 300);
        assert_eq!(Mastodon::new("https://mastodon.social", secret()).limit(), 500);
        assert_eq!(LinkedIn::new("urn:li:person:x", secret()).limit(), 3000);
    }

    #[test]
    fn an_overlong_post_is_refused_before_the_network() {
        let x = X::new(secret());
        let long = "a".repeat(281);
        match x.check_length(&long) {
            Err(PostError::TooLong { platform, len, limit }) => {
                assert_eq!(platform, "x");
                assert_eq!(len, 281);
                assert_eq!(limit, 280);
            }
            other => panic!("expected TooLong, got {other:?}"),
        }
    }

    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        // Arabic is multi-byte. Counting bytes would refuse a post that is
        // comfortably inside the limit, and the failure would look like an
        // API rejection rather than our own bug.
        let x = X::new(secret());
        let arabic = "مرحبا".repeat(50); // 250 chars, 500 bytes
        assert_eq!(arabic.chars().count(), 250);
        assert!(arabic.len() > 280, "the byte length must exceed the limit");
        assert!(x.check_length(&arabic).is_ok(), "characters, not bytes");
    }

    #[test]
    fn a_post_at_exactly_the_limit_is_allowed() {
        let x = X::new(secret());
        assert!(x.check_length(&"a".repeat(280)).is_ok());
        assert!(x.check_length(&"a".repeat(281)).is_err());
    }

    #[test]
    fn the_mastodon_instance_url_is_normalised() {
        let m = Mastodon::new("https://mastodon.social/", secret());
        assert_eq!(m.instance, "https://mastodon.social");
    }

    #[test]
    fn the_linkedin_expiry_message_says_reauthorise_not_retry() {
        // There is no refresh token, so telling an operator to retry would
        // send them in circles.
        let err = PostError::Expired {
            platform: "linkedin",
            detail: "no refresh token exists; re-authorise the app".into(),
        };
        assert!(err.to_string().contains("re-authorise"));
        assert!(!err.to_string().contains("retry"));
    }

    #[test]
    fn a_long_body_is_clipped_in_error_messages() {
        assert_eq!(clip(&"x".repeat(500)).chars().count(), 200);
    }
}
