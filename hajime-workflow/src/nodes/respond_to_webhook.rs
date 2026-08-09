//! `respondToWebhook`.
//!
//! Shapes the reply for a webhook running in `responseNode` mode. The node does
//! not write to the socket: it produces the payload, and the HTTP layer sends
//! it once the run finishes. Keeping it that way means a run started by the
//! scheduler behaves identically to one started by a request.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::expression;
use crate::model::{Item, Node};
use async_trait::async_trait;

/// Key under which the prepared reply is stored on the outgoing item.
pub const RESPONSE_KEY: &str = "__hajime_response";

#[derive(Default)]
pub struct RespondToWebhook;

#[async_trait]
impl NodeExecutor for RespondToWebhook {
    /// Puts a reply in the slot the HTTP layer reads. It sends nothing itself
    /// and starts no conversation: the only recipient is a caller already
    /// waiting on a request, and a shadow run has no such caller.
    ///
    /// Classified as a read so a rehearsal shows the real response body. That
    /// body is most of what anyone rehearsing a webhook workflow wants to see.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        // n8n replies from the first item; the rest ride along unchanged.
        let first = input.first().cloned().unwrap_or_else(Item::empty);

        let body = match node.param_str("responseBody") {
            Some(raw) => expression::resolve(raw, &first.json)
                .map_err(|e| NodeError::InvalidParameter {
                    name: "responseBody",
                    reason: e.to_string(),
                })?,
            // With no explicit body n8n echoes the incoming item, which is what
            // the `{"options": {}}` nodes in the export rely on.
            None => first.json.clone(),
        };

        let body = match node.param_str("respondWith").unwrap_or("json") {
            "json" => body,
            "text" => match body {
                serde_json::Value::String(s) => serde_json::Value::String(s),
                other => serde_json::Value::String(other.to_string()),
            },
            "noData" => serde_json::Value::Null,
            other => {
                return Err(NodeError::InvalidParameter {
                    name: "respondWith",
                    reason: format!("unsupported response type '{other}'"),
                })
            }
        };

        let mut out = first;
        if let Some(obj) = out.json.as_object_mut() {
            obj.insert(RESPONSE_KEY.to_string(), body);
        } else {
            out.json = serde_json::json!({ RESPONSE_KEY: body });
        }

        Ok(NodeOutput::items(vec![out]))
    }
}

/// Pull the prepared reply back out, for the HTTP layer.
pub fn take_response(items: &[Item]) -> Option<serde_json::Value> {
    items
        .iter()
        .find_map(|i| i.json.get(RESPONSE_KEY).cloned())
}

/// The same value with the internal marker removed.
///
/// `respondToWebhook` leaves the prepared reply on its outgoing item so the
/// HTTP layer can find it. That item is also what `responseMode: lastNode`
/// sends, so without this the caller receives Hajime's own bookkeeping key
/// alongside their data. n8n pairs that node with `responseNode` mode, but a
/// workflow can be wired either way and neither wiring should leak internals.
pub fn without_marker(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = value.as_object_mut() {
        obj.remove(RESPONSE_KEY);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(params: serde_json::Value) -> Node {
        serde_json::from_value(json!({
            "name": "Respond",
            "type": "n8n-nodes-base.respondToWebhook",
            "parameters": params,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn resolves_the_json_expression_used_in_production() {
        // The TOTP Webhook API node: {"respondWith":"json","responseBody":"={{ $json }}"}
        let n = node(json!({"respondWith": "json", "responseBody": "={{ $json }}"}));
        let input = vec![Item::from_json(json!({"code": "123456"}))];

        let out = RespondToWebhook.execute(&n, input, &ExecContext::default()).await.unwrap();
        assert_eq!(take_response(&out.items).unwrap(), json!({"code": "123456"}));
    }

    #[tokio::test]
    async fn echoes_the_item_when_no_body_is_configured() {
        // The `{"options": {}}` form used by exec-cmd and exec-via-code.
        let n = node(json!({"options": {}}));
        let input = vec![Item::from_json(json!({"stdout": "ok"}))];

        let out = RespondToWebhook.execute(&n, input, &ExecContext::default()).await.unwrap();
        assert_eq!(take_response(&out.items).unwrap(), json!({"stdout": "ok"}));
    }

    #[tokio::test]
    async fn selects_a_single_field() {
        let n = node(json!({"responseBody": "={{ $json.code }}"}));
        let input = vec![Item::from_json(json!({"code": "999", "extra": 1}))];

        let out = RespondToWebhook.execute(&n, input, &ExecContext::default()).await.unwrap();
        assert_eq!(take_response(&out.items).unwrap(), json!("999"));
    }

    #[tokio::test]
    async fn an_expression_needing_javascript_is_an_error_not_a_literal() {
        let n = node(json!({"responseBody": "={{ $json.a + 1 }}"}));
        let input = vec![Item::from_json(json!({"a": 1}))];

        let err = RespondToWebhook.execute(&n, input, &ExecContext::default()).await.unwrap_err();
        assert!(matches!(err, NodeError::InvalidParameter { name: "responseBody", .. }));
    }

    #[tokio::test]
    async fn no_data_replies_with_null() {
        let n = node(json!({"respondWith": "noData"}));
        let out = RespondToWebhook
            .execute(&n, vec![Item::from_json(json!({"a": 1}))], &ExecContext::default())
            .await
            .unwrap();
        assert_eq!(take_response(&out.items).unwrap(), serde_json::Value::Null);
    }

    #[tokio::test]
    async fn the_internal_marker_never_reaches_the_caller() {
        // Found by calling a real webhook: with `responseMode: lastNode` the
        // server sends this node's own outgoing item, which carries the key
        // the HTTP layer uses to find the reply. The caller was getting
        // Hajime's bookkeeping alongside their data.
        let n = node(json!({"options": {}}));
        let input = vec![Item::from_json(json!({"pong": true, "seen": 1}))];

        let out = RespondToWebhook.execute(&n, input, &ExecContext::default()).await.unwrap();
        let sent = without_marker(out.items[0].json.clone());

        assert_eq!(sent, json!({"pong": true, "seen": 1}));
        assert!(
            sent.get(RESPONSE_KEY).is_none(),
            "the marker survived: {sent}"
        );
        // And the HTTP layer can still find the reply it needs.
        assert!(take_response(&out.items).is_some());
    }

    #[test]
    fn stripping_a_value_that_is_not_an_object_leaves_it_alone() {
        assert_eq!(without_marker(json!("plain")), json!("plain"));
        assert_eq!(without_marker(json!(null)), json!(null));
        assert_eq!(without_marker(json!([1, 2])), json!([1, 2]));
    }

    #[tokio::test]
    async fn handles_an_empty_input_without_panicking() {
        let n = node(json!({"options": {}}));
        let out = RespondToWebhook.execute(&n, vec![], &ExecContext::default()).await.unwrap();
        assert_eq!(out.items.len(), 1);
    }
}
