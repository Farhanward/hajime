//! `httpRequest`, the most-used node in the imported workflows.
//!
//! Covers the parameter shape n8n v4 emits: `url`, `method`, `sendBody` with
//! `jsonBody`, and `sendHeaders` with `headerParameters.parameters`. A JSON
//! response is parsed into the item's `json`; anything else is carried as
//! `{"data": "<body>"}`, which is what n8n does for non-JSON payloads.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::expression;
use crate::model::{Item, Node};
use async_trait::async_trait;
use std::time::Duration;

pub struct HttpRequest {
    client: reqwest::Client,
}

impl Default for HttpRequest {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .user_agent("hajime-workflow/1.0")
                .build()
                .expect("reqwest client with default settings is always valid"),
        }
    }
}

impl HttpRequest {
    /// Resolve a parameter against the item currently being processed.
    ///
    /// n8n evaluates parameters per item, so `"url": "={{ $json.url }}"` is a
    /// different address on every pass. Reading the parameter literally would
    /// request the expression text itself.
    fn resolved(
        node: &Node,
        key: &'static str,
        item: &Item,
    ) -> Result<Option<String>, NodeError> {
        let Some(raw) = node.param_str(key) else {
            return Ok(None);
        };
        let value = expression::resolve(raw, &item.json).map_err(|e| {
            NodeError::InvalidParameter { name: key, reason: e.to_string() }
        })?;
        Ok(Some(match value {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        }))
    }

    fn headers(node: &Node) -> Vec<(String, String)> {
        let enabled = node
            .param("sendHeaders")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            return Vec::new();
        }
        node.param("headerParameters")
            .and_then(|h| h.get("parameters"))
            .and_then(|p| p.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|e| {
                        let name = e.get("name")?.as_str()?.to_string();
                        let value = e.get("value")?.as_str().unwrap_or("").to_string();
                        Some((name, value))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One request for one item.
    async fn request_once(&self, node: &Node, item: &Item) -> Result<Item, NodeError> {
        let url = Self::resolved(node, "url", item)?
            .ok_or(NodeError::MissingParameter("url"))?;

        let method_raw = node.param_str("method").unwrap_or("GET");
        let method = reqwest::Method::from_bytes(method_raw.to_uppercase().as_bytes())
            .map_err(|_| NodeError::InvalidParameter {
                name: "method",
                reason: format!("'{method_raw}' is not an HTTP method"),
            })?;

        let mut req = self.client.request(method, &url);
        for (name, value) in Self::headers(node) {
            req = req.header(name, value);
        }
        if let Some(body) = Self::body(node) {
            req = req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }

        let response = req
            .send()
            .await
            .map_err(|e| NodeError::Request(e.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| NodeError::Request(e.to_string()))?;

        let body = serde_json::from_str::<serde_json::Value>(&text)
            .unwrap_or_else(|_| serde_json::json!({ "data": text }));

        // n8n emits the response body as the item, not a wrapper. Verified
        // against stored execution 2017: no `statusCode` key appears anywhere
        // in the recorded data. The wrapper is used only when the node asks
        // for the full response.
        let full = node
            .param("options")
            .and_then(|o| o.get("fullResponse"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(Item::from_json(if full {
            serde_json::json!({ "statusCode": status, "body": body })
        } else {
            body
        }))
    }

    fn body(node: &Node) -> Option<String> {
        let enabled = node
            .param("sendBody")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            return None;
        }
        match node.param("jsonBody") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(other) => Some(other.to_string()),
            None => None,
        }
    }
}

#[async_trait]
impl NodeExecutor for HttpRequest {
    /// GET and HEAD read; everything else is assumed to change something at the
    /// other end. An unreadable method is treated as external rather than
    /// guessed: the same node posts to WhatsApp and to the social APIs.
    fn effect(&self, node: &Node) -> Effect {
        match node.param_str("method").unwrap_or("GET").trim().to_ascii_uppercase().as_str() {
            "GET" | "HEAD" | "OPTIONS" => Effect::Read,
            _ => Effect::External,
        }
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        // n8n runs this node once per incoming item. A node wired to a trigger
        // that emits nothing still fires once, so an empty list means one pass.
        let items = if input.is_empty() { vec![Item::empty()] } else { input };

        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            out.push(self.request_once(node, item).await?);
        }
        Ok(NodeOutput::items(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(params: serde_json::Value) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "Check",
            "type": "n8n-nodes-base.httpRequest",
            "parameters": params,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn requires_a_url() {
        let err = HttpRequest::default()
            .execute(
                &node(serde_json::json!({})),
                vec![],
                &ExecContext::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, NodeError::MissingParameter("url")));
    }

    #[tokio::test]
    async fn rejects_an_unusable_method() {
        let n = node(serde_json::json!({"url": "https://example.test", "method": "GET POST"}));
        let err = HttpRequest::default().execute(&n, vec![], &ExecContext::default()).await.unwrap_err();
        assert!(matches!(err, NodeError::InvalidParameter { name: "method", .. }));
    }

    #[test]
    fn reads_n8n_header_parameter_shape() {
        let n = node(serde_json::json!({
            "url": "https://example.test",
            "sendHeaders": true,
            "headerParameters": {"parameters": [
                {"name": "X-Token", "value": "abc"},
                {"name": "Accept", "value": "application/json"}
            ]}
        }));
        assert_eq!(
            HttpRequest::headers(&n),
            vec![
                ("X-Token".to_string(), "abc".to_string()),
                ("Accept".to_string(), "application/json".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_headers_when_the_toggle_is_off() {
        let n = node(serde_json::json!({
            "url": "https://example.test",
            "headerParameters": {"parameters": [{"name": "X", "value": "y"}]}
        }));
        assert!(HttpRequest::headers(&n).is_empty());
    }

    #[test]
    fn resolves_a_url_expression_per_item() {
        // The shape used by the Hourly Health Monitor: one URL per input item.
        let n = node(serde_json::json!({"url": "={{ $json.url }}"}));
        let a = Item::from_json(serde_json::json!({"url": "https://a.test/x"}));
        let b = Item::from_json(serde_json::json!({"url": "https://b.test/y"}));

        assert_eq!(
            HttpRequest::resolved(&n, "url", &a).unwrap().as_deref(),
            Some("https://a.test/x")
        );
        assert_eq!(
            HttpRequest::resolved(&n, "url", &b).unwrap().as_deref(),
            Some("https://b.test/y")
        );
    }

    #[test]
    fn a_literal_url_is_left_alone() {
        let n = node(serde_json::json!({"url": "https://example.test/health"}));
        assert_eq!(
            HttpRequest::resolved(&n, "url", &Item::empty()).unwrap().as_deref(),
            Some("https://example.test/health")
        );
    }

    #[test]
    fn an_expression_that_cannot_resolve_is_an_error() {
        let n = node(serde_json::json!({"url": "={{ $json.missing }}"}));
        let err = HttpRequest::resolved(&n, "url", &Item::empty()).unwrap_err();
        assert!(matches!(err, NodeError::InvalidParameter { name: "url", .. }));
    }

    #[test]
    fn sends_a_body_only_when_requested() {
        let off = node(serde_json::json!({"url": "u", "jsonBody": "{\"a\":1}"}));
        assert_eq!(HttpRequest::body(&off), None);

        let on = node(serde_json::json!({
            "url": "u", "sendBody": true, "jsonBody": "{\"a\":1}"
        }));
        assert_eq!(HttpRequest::body(&on).as_deref(), Some("{\"a\":1}"));
    }
}
