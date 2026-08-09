//! `rssFeedRead`.
//!
//! Fetches a feed and emits one item per entry. `feed-rs` handles RSS 2.0,
//! RSS 1.0 and Atom behind one parser, which matters because the configured
//! source (Google Trends) has changed format before.
//!
//! Field names follow n8n's output so downstream Code nodes keep working:
//! `title`, `link`, `pubDate`, `content`, `contentSnippet`, `guid`, `isoDate`.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::model::{Item, Node};
use async_trait::async_trait;
use std::time::Duration;

pub struct RssFeedRead {
    client: reqwest::Client,
}

impl Default for RssFeedRead {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(45))
                .user_agent("hajime-workflow/1.0")
                .build()
                .expect("reqwest client with default settings is always valid"),
        }
    }
}

impl RssFeedRead {
    /// Split out so the parsing can be tested without a network call.
    pub fn parse(bytes: &[u8]) -> Result<Vec<Item>, NodeError> {
        let feed = feed_rs::parser::parse(bytes)
            .map_err(|e| NodeError::Other(format!("could not parse feed: {e}")))?;

        Ok(feed
            .entries
            .into_iter()
            .map(|entry| {
                let title = entry.title.map(|t| t.content).unwrap_or_default();
                let link = entry.links.first().map(|l| l.href.clone()).unwrap_or_default();
                let content = entry
                    .content
                    .and_then(|c| c.body)
                    .or_else(|| entry.summary.map(|s| s.content))
                    .unwrap_or_default();
                let published = entry.published.or(entry.updated);

                Item::from_json(serde_json::json!({
                    "title": title,
                    "link": link,
                    "guid": entry.id,
                    "content": content,
                    "contentSnippet": snippet(&content),
                    "pubDate": published.map(|d| d.to_rfc2822()),
                    "isoDate": published.map(|d| d.to_rfc3339()),
                }))
            })
            .collect())
    }
}

/// n8n's `contentSnippet` is the text with markup removed, trimmed short.
fn snippet(content: &str) -> String {
    let mut text = String::with_capacity(content.len());
    let mut in_tag = false;
    for c in content.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 280 {
        text.chars().take(280).collect()
    } else {
        text
    }
}

#[async_trait]
impl NodeExecutor for RssFeedRead {
    /// Fetches a feed. Reads only.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        node: &Node,
        _input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        let url = node
            .param_str("url")
            .ok_or(NodeError::MissingParameter("url"))?;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| NodeError::Request(e.to_string()))?;

        if !response.status().is_success() {
            return Err(NodeError::Request(format!(
                "feed returned HTTP {}",
                response.status().as_u16()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| NodeError::Request(e.to_string()))?;

        Ok(NodeOutput::items(Self::parse(&bytes)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the Google Trends feed the SEO workflow reads.
    const RSS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>Daily Search Trends</title>
  <item>
    <title>Riyadh Season</title>
    <link>https://trends.google.com/trends/x</link>
    <guid>tag:trends,2026:1</guid>
    <pubDate>Mon, 03 Aug 2026 04:00:00 GMT</pubDate>
    <description>&lt;p&gt;Some  &lt;b&gt;markup&lt;/b&gt; here&lt;/p&gt;</description>
  </item>
  <item>
    <title>Second Topic</title>
    <link>https://trends.google.com/trends/y</link>
    <guid>tag:trends,2026:2</guid>
  </item>
</channel></rss>"#;

    #[test]
    fn emits_one_item_per_entry() {
        let items = RssFeedRead::parse(RSS).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].json["title"], "Riyadh Season");
        assert_eq!(items[1].json["title"], "Second Topic");
    }

    #[test]
    fn maps_the_field_names_n8n_downstream_code_expects() {
        let items = RssFeedRead::parse(RSS).unwrap();
        let first = &items[0].json;
        for key in ["title", "link", "guid", "content", "contentSnippet", "pubDate", "isoDate"] {
            assert!(first.get(key).is_some(), "missing field {key}");
        }
        assert_eq!(first["link"], "https://trends.google.com/trends/x");
    }

    #[test]
    fn snippet_strips_markup_and_collapses_whitespace() {
        assert_eq!(snippet("<p>Some  <b>markup</b> here</p>"), "Some markup here");
    }

    #[test]
    fn snippet_is_capped() {
        let long = "x".repeat(500);
        assert_eq!(snippet(&long).chars().count(), 280);
    }

    #[test]
    fn an_entry_without_a_date_still_parses() {
        let items = RssFeedRead::parse(RSS).unwrap();
        assert!(items[1].json["pubDate"].is_null());
    }

    #[test]
    fn malformed_xml_is_an_error_not_an_empty_list() {
        let err = RssFeedRead::parse(b"not a feed at all").unwrap_err();
        assert!(matches!(err, NodeError::Other(_)));
    }

    #[tokio::test]
    async fn requires_a_url() {
        let n: Node = serde_json::from_value(serde_json::json!({
            "name": "RSS", "type": "n8n-nodes-base.rssFeedRead", "parameters": {}
        }))
        .unwrap();
        let err = RssFeedRead::default().execute(&n, vec![], &ExecContext::default()).await.unwrap_err();
        assert!(matches!(err, NodeError::MissingParameter("url")));
    }
}
