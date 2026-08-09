//! Node executors.
//!
//! Each n8n node kind maps to one [`NodeExecutor`]. A kind with no executor is
//! treated as pass-through by the engine, so a workflow can be ported one node
//! at a time instead of all at once.

pub mod code;
pub mod execute_command;
pub mod http_request;
pub mod read_write_file;
pub mod respond_to_webhook;
pub mod rss_feed_read;
pub mod ssh;
pub mod triggers;

use crate::model::{Item, Node};
pub use hajime_core::tools::Effect;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("missing required parameter '{0}'")]
    MissingParameter(&'static str),
    #[error("invalid parameter '{name}': {reason}")]
    InvalidParameter { name: &'static str, reason: String },
    #[error("request failed: {0}")]
    Request(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug)]
pub struct NodeOutput {
    pub items: Vec<Item>,
}

impl NodeOutput {
    pub fn items(items: Vec<Item>) -> Self {
        Self { items }
    }

    pub fn single(json: serde_json::Value) -> Self {
        Self { items: vec![Item::from_json(json)] }
    }
}

/// What a node can see beyond its own input.
///
/// n8n's Code node can reach any already-executed node by name through
/// `$items('Node Name')`, so executors need more than their direct input.
#[derive(Default)]
pub struct ExecContext<'a> {
    outputs: Option<&'a HashMap<String, Vec<Item>>>,
}

impl<'a> ExecContext<'a> {
    pub fn new(outputs: &'a HashMap<String, Vec<Item>>) -> Self {
        Self { outputs: Some(outputs) }
    }

    /// Items produced by an earlier node, or `None` if it has not run.
    pub fn node_items(&self, name: &str) -> Option<&Vec<Item>> {
        self.outputs.and_then(|o| o.get(name))
    }

    /// Every completed node keyed by name, for `$items(..)` in the Code node.
    pub fn sibling_map(&self) -> HashMap<&str, &Vec<Item>> {
        self.outputs
            .map(|o| o.iter().map(|(k, v)| (k.as_str(), v)).collect())
            .unwrap_or_default()
    }
}

#[async_trait]
pub trait NodeExecutor: Send + Sync {
    /// Run once for the whole input list, matching n8n's execution model.
    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError>;

    /// What running this node does to the world beyond the process.
    ///
    /// Per node rather than per kind, because the answer often depends on the
    /// parameters: an httpRequest doing a GET reads, and the same node with the
    /// method changed to POST does not.
    ///
    /// The default is the most dangerous answer. A node kind added later and
    /// not classified here is held back by a shadow run rather than let
    /// through, which is the failure that can be noticed rather than the one
    /// that cannot.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::External
    }
}

#[derive(Clone, Default)]
pub struct Registry {
    executors: HashMap<String, Arc<dyn NodeExecutor>>,
}

impl Registry {
    /// No executors: every node passes through. Used by the engine tests to
    /// exercise graph walking on its own.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Every kind the imported workflows use.
    ///
    /// The three host-touching kinds are registered unconditionally but refuse
    /// to act unless `policy` permits it, so `/status` reports them as
    /// supported and a disabled capability fails loudly instead of silently
    /// passing data through.
    pub fn with_builtins() -> Self {
        Self::with_policy(crate::policy::Policy::default(), None)
    }

    pub fn with_policy(
        policy: crate::policy::Policy,
        ssh_target: Option<ssh::SshTarget>,
    ) -> Self {
        let mut reg = Self::default();
        reg.register(
            "executeCommand",
            Arc::new(execute_command::ExecuteCommand::new(policy.clone())),
        );
        reg.register(
            "readWriteFile",
            Arc::new(read_write_file::ReadWriteFile::new(policy.clone())),
        );
        reg.register("ssh", Arc::new(ssh::Ssh::new(policy, ssh_target)));
        reg.register("code", Arc::new(code::Code));
        reg.register("httpRequest", Arc::new(http_request::HttpRequest::default()));
        reg.register("rssFeedRead", Arc::new(rss_feed_read::RssFeedRead::default()));
        reg.register(
            "respondToWebhook",
            Arc::new(respond_to_webhook::RespondToWebhook),
        );
        reg.register("scheduleTrigger", Arc::new(triggers::ScheduleTrigger));
        reg.register("webhook", Arc::new(triggers::Webhook));
        reg.register(
            "executeWorkflowTrigger",
            Arc::new(triggers::ExecuteWorkflowTrigger),
        );
        reg.register("manualTrigger", Arc::new(triggers::ManualTrigger));
        reg
    }

    pub fn register(&mut self, kind: &str, exec: Arc<dyn NodeExecutor>) {
        self.executors.insert(kind.to_string(), exec);
    }

    pub fn get(&self, kind: &str) -> Option<&Arc<dyn NodeExecutor>> {
        self.executors.get(kind)
    }

    pub fn kinds(&self) -> Vec<&str> {
        let mut k: Vec<&str> = self.executors.keys().map(String::as_str).collect();
        k.sort_unstable();
        k
    }
}
