//! `readWriteFile`.
//!
//! Reads a file into an item, or writes an item's property to disk. Confined
//! to the roots declared in [`Policy::file_roots`]; with none declared the node
//! refuses everything. The real workflow writes to `/vault/secrets/`, so a
//! deployment that needs it names that directory explicitly rather than handing
//! the service the whole filesystem.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::expression;
use crate::model::{Item, Node};
use crate::policy::Policy;
use std::path::Path;
use async_trait::async_trait;
use std::path::PathBuf;

pub struct ReadWriteFile {
    policy: Policy,
}

impl ReadWriteFile {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    fn file_name(node: &Node, item: &Item) -> Result<PathBuf, NodeError> {
        let raw = node
            .param_str("fileName")
            .ok_or(NodeError::MissingParameter("fileName"))?;
        let value = expression::resolve(raw, &item.json).map_err(|e| {
            NodeError::InvalidParameter { name: "fileName", reason: e.to_string() }
        })?;
        match value {
            serde_json::Value::String(s) if !s.trim().is_empty() => Ok(PathBuf::from(s)),
            _ => Err(NodeError::InvalidParameter {
                name: "fileName",
                reason: "resolved to an empty path".into(),
            }),
        }
    }

    fn guard(&self, path: &Path) -> Result<(), NodeError> {
        if self.policy.file_allowed(path) {
            return Ok(());
        }
        Err(NodeError::Other(if self.policy.file_roots.is_empty() {
            "readWriteFile is disabled. Set HAJIME_FILE_ROOTS to the directories \
             this service may touch."
                .to_string()
        } else {
            format!(
                "'{}' is outside the permitted roots {:?}",
                path.display(),
                self.policy.file_roots
            )
        }))
    }
}

#[async_trait]
impl NodeExecutor for ReadWriteFile {
    /// Reading a file is a read; writing one changes the machine. The operation
    /// is a parameter, so the answer has to be read from the node.
    fn effect(&self, node: &Node) -> Effect {
        match node.param_str("operation").unwrap_or("read") {
            "read" => Effect::Read,
            _ => Effect::Local,
        }
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        let operation = node.param_str("operation").unwrap_or("read");
        let property = node.param_str("dataPropertyName").unwrap_or("data");
        let items = if input.is_empty() { vec![Item::empty()] } else { input };

        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            let path = Self::file_name(node, item)?;
            self.guard(&path)?;

            match operation {
                "read" => {
                    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
                        NodeError::Other(format!("could not stat {}: {e}", path.display()))
                    })?;
                    if meta.len() as usize > self.policy.max_bytes {
                        return Err(NodeError::Other(format!(
                            "{} is {} bytes, above the {} byte limit",
                            path.display(),
                            meta.len(),
                            self.policy.max_bytes
                        )));
                    }
                    let bytes = tokio::fs::read(&path).await.map_err(|e| {
                        NodeError::Other(format!("could not read {}: {e}", path.display()))
                    })?;
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    let mut json = item.json.clone();
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(property.to_string(), serde_json::Value::String(text));
                        obj.insert(
                            "fileName".to_string(),
                            serde_json::Value::String(path.display().to_string()),
                        );
                    }
                    out.push(Item::from_json(json));
                }
                "write" => {
                    let content = match item.json.get(property) {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => {
                            return Err(NodeError::InvalidParameter {
                                name: "dataPropertyName",
                                reason: format!("the item has no '{property}' property"),
                            })
                        }
                    };
                    if let Some(parent) = path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            NodeError::Other(format!(
                                "could not create {}: {e}",
                                parent.display()
                            ))
                        })?;
                    }
                    tokio::fs::write(&path, content.as_bytes()).await.map_err(|e| {
                        NodeError::Other(format!("could not write {}: {e}", path.display()))
                    })?;
                    out.push(Item::from_json(serde_json::json!({
                        "fileName": path.display().to_string(),
                        "bytesWritten": content.len(),
                    })));
                }
                other => {
                    return Err(NodeError::InvalidParameter {
                        name: "operation",
                        reason: format!("unknown operation '{other}'"),
                    })
                }
            }
        }
        Ok(NodeOutput::items(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(params: serde_json::Value) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "File",
            "type": "n8n-nodes-base.readWriteFile",
            "parameters": params,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn refused_when_no_roots_are_declared() {
        let n = node(serde_json::json!({"operation": "read", "fileName": "/etc/passwd"}));
        let err = ReadWriteFile::new(Policy::default())
            .execute(&n, vec![], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HAJIME_FILE_ROOTS"), "got: {err}");
    }

    #[tokio::test]
    async fn refuses_a_path_outside_the_roots() {
        let dir = std::env::temp_dir().join("hajime_rw_out");
        let n = node(serde_json::json!({"operation": "read", "fileName": "/etc/passwd"}));
        let err = ReadWriteFile::new(Policy::permissive(vec![dir]))
            .execute(&n, vec![], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside the permitted roots"), "got: {err}");
    }

    #[tokio::test]
    async fn writes_then_reads_the_same_file() {
        let dir = std::env::temp_dir().join("hajime_rw_test");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        let path = dir.join("out.txt");
        let policy = Policy::permissive(vec![dir.clone()]);

        let write = node(serde_json::json!({
            "operation": "write",
            "fileName": path.display().to_string(),
            "dataPropertyName": "data"
        }));
        let input = vec![Item::from_json(serde_json::json!({"data": "yokoso"}))];
        let result = ReadWriteFile::new(policy.clone())
            .execute(&write, input, &ExecContext::default())
            .await
            .unwrap();
        assert_eq!(result.items[0].json["bytesWritten"], 6);

        let read = node(serde_json::json!({
            "operation": "read",
            "fileName": path.display().to_string(),
            "dataPropertyName": "data"
        }));
        let result = ReadWriteFile::new(policy)
            .execute(&read, vec![], &ExecContext::default())
            .await
            .unwrap();
        assert_eq!(result.items[0].json["data"], "yokoso");

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn traversal_out_of_a_root_is_refused() {
        let dir = std::env::temp_dir().join("hajime_rw_trav");
        let escape = dir.join("../../etc/passwd");
        let n = node(serde_json::json!({
            "operation": "read", "fileName": escape.display().to_string()
        }));
        let err = ReadWriteFile::new(Policy::permissive(vec![dir]))
            .execute(&n, vec![], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("outside the permitted roots"), "got: {err}");
    }

    #[tokio::test]
    async fn writing_without_the_named_property_is_an_error() {
        let dir = std::env::temp_dir().join("hajime_rw_missing");
        let n = node(serde_json::json!({
            "operation": "write",
            "fileName": dir.join("x.txt").display().to_string(),
            "dataPropertyName": "data"
        }));
        let err = ReadWriteFile::new(Policy::permissive(vec![dir]))
            .execute(&n, vec![Item::from_json(serde_json::json!({"other": 1}))], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            NodeError::InvalidParameter { name: "dataPropertyName", .. }
        ));
    }
}
