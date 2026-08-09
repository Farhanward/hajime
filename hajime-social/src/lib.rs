//! Social publishing tools.
//!
//! Replaces what Postiz did: schedule was already covered by
//! `hajime-workflow`, so what remained was knowing each platform's API. That
//! is 1.05 GB of Node, Temporal and Elasticsearch reduced to a few hundred
//! lines, because the scheduling engine was the expensive part and we already
//! had one.
//!
//! Tools are described in MCP's shape (`name`, `description`, `inputSchema`),
//! so the model discovers them the same way it would discover a third-party
//! MCP server's tools. They link in-process rather than crossing an MCP
//! transport: these are our own tools, and an extra process per capability
//! costs memory on a machine that does not have it to spare. The transport is
//! for tools we did not write.

pub mod platform;

use async_trait::async_trait;
use hajime_core::tools::{ToolProvider, ToolSpec};
use hajime_core::secrets;
use platform::{Bluesky, LinkedIn, Mastodon, Platform, PostError, X};
use std::collections::HashMap;
use std::sync::Arc;

/// Every publishing tool, keyed by tool name.
pub struct Social {
    platforms: HashMap<String, Arc<dyn Platform>>,
}

impl Social {
    pub fn empty() -> Self {
        Self { platforms: HashMap::new() }
    }

    /// Build from the secret store.
    ///
    /// A platform whose secret is absent is simply not offered. The model can
    /// only see tools that would actually work, so it never picks one that is
    /// guaranteed to fail.
    pub fn from_secrets(store: &secrets::Store) -> Self {
        let mut platforms: HashMap<String, Arc<dyn Platform>> = HashMap::new();

        if let Some(token) = store.try_get("mastodon_token") {
            let instance = store
                .try_get("mastodon_instance")
                .map(|s| s.expose().to_string())
                .unwrap_or_else(|| "https://mastodon.social".to_string());
            platforms.insert(
                "post_to_mastodon".into(),
                Arc::new(Mastodon::new(instance, token.clone())),
            );
        }
        if let Some(token) = store.try_get("x_token") {
            platforms.insert("post_to_x".into(), Arc::new(X::new(token.clone())));
        }
        if let (Some(token), Some(handle)) =
            (store.try_get("bluesky_token"), store.try_get("bluesky_handle"))
        {
            platforms.insert(
                "post_to_bluesky".into(),
                Arc::new(Bluesky::new(handle.expose(), token.clone())),
            );
        }
        if let (Some(token), Some(urn)) =
            (store.try_get("linkedin_token"), store.try_get("linkedin_urn"))
        {
            platforms.insert(
                "post_to_linkedin".into(),
                Arc::new(LinkedIn::new(urn.expose(), token.clone())),
            );
        }

        Self { platforms }
    }

    pub fn insert(&mut self, tool: &str, platform: Arc<dyn Platform>) {
        self.platforms.insert(tool.to_string(), platform);
    }

    pub fn configured(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.platforms.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

fn spec_for(tool: &str, platform: &dyn Platform) -> ToolSpec {
    ToolSpec::external(
        tool,
        &format!(
            "Publish a post to {}. Maximum {} characters. The post is public \
             and cannot be unpublished by this tool.",
            platform.name(),
            platform.limit()
        ),
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": format!(
                        "The post body, at most {} characters",
                        platform.limit()
                    ),
                }
            },
            "required": ["text"],
        }),
    )
}

#[async_trait]
impl ToolProvider for Social {
    fn name(&self) -> &str {
        "social"
    }

    async fn list(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .platforms
            .iter()
            .map(|(tool, p)| spec_for(tool, p.as_ref()))
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let Some(platform) = self.platforms.get(tool) else {
            return Err(format!("no publishing tool named '{tool}'"));
        };

        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "the 'text' argument is required".to_string())?;
        if text.trim().is_empty() {
            return Err("the post text is empty".into());
        }

        match platform.post(text).await {
            Ok(posted) => Ok(serde_json::to_value(posted).unwrap_or_default()),
            // The message reaches the model, so it has to be actionable rather
            // than a status code. An expired credential is not something the
            // model can retry its way out of.
            Err(e @ PostError::Expired { .. }) => {
                Err(format!("{e}. This needs a human; do not retry."))
            }
            Err(e @ PostError::TooLong { .. }) => {
                Err(format!("{e}. Shorten the text and call again."))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hajime_core::tools::{Budget, Effect, Gateway};
    use std::sync::Mutex;

    struct StubPlatform {
        name: &'static str,
        limit: usize,
        posted: Mutex<Vec<String>>,
        fail: Option<PostError>,
    }

    impl StubPlatform {
        fn ok(name: &'static str, limit: usize) -> Self {
            Self { name, limit, posted: Mutex::new(Vec::new()), fail: None }
        }
        fn failing(name: &'static str, err: PostError) -> Self {
            Self { name, limit: 280, posted: Mutex::new(Vec::new()), fail: Some(err) }
        }
        fn sent(&self) -> Vec<String> {
            self.posted.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Platform for StubPlatform {
        fn name(&self) -> &'static str {
            self.name
        }
        fn limit(&self) -> usize {
            self.limit
        }
        async fn post(&self, text: &str) -> Result<platform::Posted, PostError> {
            self.check_length(text)?;
            if let Some(e) = &self.fail {
                return Err(match e {
                    PostError::Expired { platform, detail } => PostError::Expired {
                        platform,
                        detail: detail.clone(),
                    },
                    other => PostError::Rejected {
                        platform: self.name,
                        reason: other.to_string(),
                    },
                });
            }
            self.posted.lock().unwrap().push(text.to_string());
            Ok(platform::Posted {
                platform: self.name.into(),
                id: "123".into(),
                url: None,
            })
        }
    }

    fn social_with(tool: &str, p: Arc<dyn Platform>) -> Social {
        let mut s = Social::empty();
        s.insert(tool, p);
        s
    }

    #[tokio::test]
    async fn only_configured_platforms_are_offered() {
        let s = social_with("post_to_x", Arc::new(StubPlatform::ok("x", 280)));
        let tools = s.list().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "post_to_x");
        assert_eq!(s.configured(), vec!["post_to_x"]);
    }

    #[tokio::test]
    async fn an_empty_store_offers_nothing() {
        assert!(Social::empty().list().await.is_empty());
    }

    #[tokio::test]
    async fn every_publishing_tool_declares_an_external_effect() {
        // If one were mislabelled, dry run would let it publish for real.
        let mut s = Social::empty();
        s.insert("post_to_x", Arc::new(StubPlatform::ok("x", 280)));
        s.insert("post_to_mastodon", Arc::new(StubPlatform::ok("mastodon", 500)));

        for spec in s.list().await {
            assert_eq!(spec.effect, Effect::External, "{} must be external", spec.name);
        }
    }

    #[tokio::test]
    async fn the_description_carries_the_character_limit() {
        let s = social_with("post_to_x", Arc::new(StubPlatform::ok("x", 280)));
        let spec = &s.list().await[0];
        assert!(spec.description.contains("280"));
        assert!(spec.description.contains("cannot be unpublished"));
        assert_eq!(spec.input_schema["required"][0], "text");
    }

    #[tokio::test]
    async fn a_post_reaches_the_platform() {
        let stub = Arc::new(StubPlatform::ok("x", 280));
        let s = social_with("post_to_x", stub.clone());

        let out = s
            .call("post_to_x", serde_json::json!({"text": "مرحبا بالعالم"}))
            .await
            .unwrap();
        assert_eq!(out["platform"], "x");
        assert_eq!(stub.sent(), vec!["مرحبا بالعالم"]);
    }

    #[tokio::test]
    async fn missing_or_empty_text_is_refused_without_posting() {
        let stub = Arc::new(StubPlatform::ok("x", 280));
        let s = social_with("post_to_x", stub.clone());

        assert!(s.call("post_to_x", serde_json::json!({})).await.is_err());
        assert!(s
            .call("post_to_x", serde_json::json!({"text": "   "}))
            .await
            .is_err());
        assert!(stub.sent().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_named_in_the_error() {
        let s = Social::empty();
        let err = s.call("post_to_nowhere", serde_json::json!({"text": "x"})).await.unwrap_err();
        assert!(err.contains("post_to_nowhere"));
    }

    #[tokio::test]
    async fn an_expired_credential_tells_the_model_not_to_retry() {
        let s = social_with(
            "post_to_linkedin",
            Arc::new(StubPlatform::failing(
                "linkedin",
                PostError::Expired {
                    platform: "linkedin",
                    detail: "no refresh token exists; re-authorise the app".into(),
                },
            )),
        );
        let err = s
            .call("post_to_linkedin", serde_json::json!({"text": "hi"}))
            .await
            .unwrap_err();
        assert!(err.contains("re-authorise"), "got: {err}");
        assert!(err.contains("do not retry"), "got: {err}");
    }

    #[tokio::test]
    async fn an_overlong_post_tells_the_model_to_shorten_it() {
        let s = social_with("post_to_x", Arc::new(StubPlatform::ok("x", 280)));
        let err = s
            .call("post_to_x", serde_json::json!({"text": "a".repeat(300)}))
            .await
            .unwrap_err();
        assert!(err.contains("Shorten"), "got: {err}");
    }

    #[tokio::test]
    async fn dry_run_stops_a_real_post() {
        // The end-to-end guarantee: with the gateway in dry-run mode, nothing
        // reaches the platform.
        let stub = Arc::new(StubPlatform::ok("x", 280));
        let mut gateway = Gateway::new(Budget::default(), true);
        gateway.add(Box::new(social_with("post_to_x", stub.clone())));

        let out = gateway
            .call("model", "post_to_x", serde_json::json!({"text": "live post"}))
            .await
            .unwrap();

        assert_eq!(out["dryRun"], true);
        assert!(stub.sent().is_empty(), "nothing may reach the platform in dry run");
    }

    #[tokio::test]
    async fn live_mode_lets_the_post_through() {
        let stub = Arc::new(StubPlatform::ok("x", 280));
        let mut gateway = Gateway::new(Budget::default(), false);
        gateway.add(Box::new(social_with("post_to_x", stub.clone())));

        gateway
            .call("model", "post_to_x", serde_json::json!({"text": "live post"}))
            .await
            .unwrap();
        assert_eq!(stub.sent(), vec!["live post"]);
    }

    #[tokio::test]
    async fn the_gateway_budget_caps_publishing() {
        let stub = Arc::new(StubPlatform::ok("x", 280));
        let mut gateway = Gateway::new(
            Budget { per_tool: 2, per_run: 10, failures_before_open: 9 },
            false,
        );
        gateway.add(Box::new(social_with("post_to_x", stub.clone())));

        for _ in 0..2 {
            gateway
                .call("m", "post_to_x", serde_json::json!({"text": "x"}))
                .await
                .unwrap();
        }
        assert!(gateway
            .call("m", "post_to_x", serde_json::json!({"text": "x"}))
            .await
            .is_err());
        assert_eq!(stub.sent().len(), 2, "the budget must stop the third post");
    }
}
