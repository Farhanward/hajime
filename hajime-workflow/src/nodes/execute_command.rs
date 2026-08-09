//! `executeCommand`.
//!
//! Runs a shell command on the host. Refused unless [`Policy::allow_command`]
//! is set, because one of the imported workflows passes the webhook body
//! straight through (`"command": "={{ $json.body.cmd }}"`). Building that node
//! without a switch would turn any exposed webhook into a shell.
//!
//! Output matches n8n: `{stdout, stderr, exitCode}`.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::expression;
use crate::model::{Item, Node};
use crate::policy::Policy;
use async_trait::async_trait;
use std::time::Duration;

pub struct ExecuteCommand {
    policy: Policy,
}

impl ExecuteCommand {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    fn command_for(node: &Node, item: &Item) -> Result<String, NodeError> {
        let raw = node
            .param_str("command")
            .ok_or(NodeError::MissingParameter("command"))?;
        let value = expression::resolve(raw, &item.json).map_err(|e| {
            NodeError::InvalidParameter { name: "command", reason: e.to_string() }
        })?;
        match value {
            serde_json::Value::String(s) if !s.trim().is_empty() => Ok(s),
            serde_json::Value::String(_) => Err(NodeError::InvalidParameter {
                name: "command",
                reason: "resolved to an empty command".into(),
            }),
            other => Ok(other.to_string()),
        }
    }

    async fn run_one(&self, command: &str) -> Result<Item, NodeError> {
        // The parameter is a full shell line in every imported workflow, so it
        // goes to a shell rather than being split into argv.
        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(command);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.arg("-c").arg(command);
            c
        };

        let child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| NodeError::Other(format!("could not start command: {e}")))?;

        let timeout = Duration::from_secs(self.policy.command_timeout_secs);
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                NodeError::Other(format!(
                    "command exceeded {}s and was abandoned",
                    self.policy.command_timeout_secs
                ))
            })?
            .map_err(|e| NodeError::Other(format!("command failed: {e}")))?;

        let clip = |bytes: Vec<u8>| {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if text.len() > self.policy.max_bytes {
                text.chars().take(self.policy.max_bytes).collect()
            } else {
                text
            }
        };

        Ok(Item::from_json(serde_json::json!({
            "stdout": clip(output.stdout),
            "stderr": clip(output.stderr),
            "exitCode": output.status.code().unwrap_or(-1),
        })))
    }
}

#[async_trait]
impl NodeExecutor for ExecuteCommand {
    /// A shell on the host. Nothing is more external than this.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::External
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        if !self.policy.allow_command {
            return Err(NodeError::Other(
                "executeCommand is disabled. Set HAJIME_ALLOW_COMMAND=1 to enable \
                 it, and only once you are satisfied no exposed webhook can reach \
                 this node."
                    .into(),
            ));
        }

        let items = if input.is_empty() { vec![Item::empty()] } else { input };
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            let command = Self::command_for(node, item)?;
            out.push(self.run_one(&command).await?);
        }
        Ok(NodeOutput::items(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(command: &str) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "Run",
            "type": "n8n-nodes-base.executeCommand",
            "parameters": {"command": command},
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn refused_unless_explicitly_enabled() {
        let err = ExecuteCommand::new(Policy::default())
            .execute(&node("echo hi"), vec![], &ExecContext::default())
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("disabled"), "got: {text}");
        assert!(text.contains("HAJIME_ALLOW_COMMAND"), "got: {text}");
    }

    #[tokio::test]
    async fn runs_a_command_and_reports_the_streams() {
        let exec = ExecuteCommand::new(Policy::permissive(vec![]));
        let out = exec
            .execute(&node("echo hajime"), vec![], &ExecContext::default())
            .await
            .unwrap();
        assert_eq!(out.items.len(), 1);
        assert!(out.items[0].json["stdout"].as_str().unwrap().contains("hajime"));
        assert_eq!(out.items[0].json["exitCode"], 0);
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_code_rather_than_erroring() {
        let exec = ExecuteCommand::new(Policy::permissive(vec![]));
        let out = exec
            .execute(&node("exit 3"), vec![], &ExecContext::default())
            .await
            .unwrap();
        assert_eq!(out.items[0].json["exitCode"], 3);
    }

    #[test]
    fn resolves_the_webhook_body_expression_used_by_exec_cmd() {
        // The real parameter from the exec-cmd workflow.
        let n = node("={{ $json.body.cmd }}");
        let item = Item::from_json(serde_json::json!({"body": {"cmd": "uname -a"}}));
        assert_eq!(ExecuteCommand::command_for(&n, &item).unwrap(), "uname -a");
    }

    #[test]
    fn an_empty_resolved_command_is_refused() {
        let n = node("={{ $json.body.cmd }}");
        let item = Item::from_json(serde_json::json!({"body": {"cmd": "   "}}));
        assert!(matches!(
            ExecuteCommand::command_for(&n, &item),
            Err(NodeError::InvalidParameter { name: "command", .. })
        ));
    }

    #[tokio::test]
    async fn runs_once_per_input_item() {
        let exec = ExecuteCommand::new(Policy::permissive(vec![]));
        let n = node("echo one");
        let input = vec![Item::empty(), Item::empty(), Item::empty()];
        let out = exec.execute(&n, input, &ExecContext::default()).await.unwrap();
        assert_eq!(out.items.len(), 3);
    }
}
