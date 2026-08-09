//! `ssh`.
//!
//! Runs a command on another host. Off unless [`Policy::allow_ssh`] is set:
//! the imported workflow feeds it the webhook body verbatim
//! (`"command": "={{ $json.body.cmd }}"`), which is remote code execution on a
//! second machine.
//!
//! Credentials come from configuration, not from the workflow file. n8n keeps
//! them in its own encrypted store, which this service deliberately does not
//! read, so a workflow export carries no secrets.
//!
//!   HAJIME_SSH_HOST, HAJIME_SSH_PORT, HAJIME_SSH_USER
//!   HAJIME_SSH_PASSWORD   or   HAJIME_SSH_KEY (path to a private key)
//!   HAJIME_SSH_FINGERPRINT  required: the server's expected public key
//!
//! The fingerprint is mandatory. Accepting whatever key answers would leave
//! every command open to interception, and this node exists to run privileged
//! commands.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::expression;
use crate::model::{Item, Node};
use crate::policy::Policy;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub key_path: Option<String>,
    /// Base64 SHA256 fingerprint of the server's public key, as printed by
    /// `ssh-keygen -lf <host_key>`.
    pub fingerprint: String,
}

impl SshTarget {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("HAJIME_SSH_HOST").ok()?;
        let fingerprint = std::env::var("HAJIME_SSH_FINGERPRINT").ok()?;
        Some(Self {
            host,
            port: std::env::var("HAJIME_SSH_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(22),
            user: std::env::var("HAJIME_SSH_USER").unwrap_or_else(|_| "root".into()),
            password: std::env::var("HAJIME_SSH_PASSWORD").ok(),
            key_path: std::env::var("HAJIME_SSH_KEY").ok(),
            fingerprint,
        })
    }
}

/// Verifies the server key against the configured fingerprint.
struct Verifier {
    expected: String,
}

impl russh::client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let actual = key.fingerprint(Default::default()).to_string();
        // `ssh-keygen -l` prints `SHA256:<base64>`; accept either form.
        let expected = self.expected.trim();
        Ok(actual == expected
            || actual.trim_start_matches("SHA256:") == expected.trim_start_matches("SHA256:"))
    }
}

pub struct Ssh {
    policy: Policy,
    target: Option<SshTarget>,
}

impl Ssh {
    pub fn new(policy: Policy, target: Option<SshTarget>) -> Self {
        Self { policy, target }
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
            _ => Err(NodeError::InvalidParameter {
                name: "command",
                reason: "resolved to an empty command".into(),
            }),
        }
    }

    async fn run_one(&self, target: &SshTarget, command: &str) -> Result<Item, NodeError> {
        let config = Arc::new(russh::client::Config {
            inactivity_timeout: Some(Duration::from_secs(self.policy.command_timeout_secs)),
            ..Default::default()
        });

        let verifier = Verifier { expected: target.fingerprint.clone() };
        let mut session =
            russh::client::connect(config, (target.host.as_str(), target.port), verifier)
                .await
                .map_err(|e| NodeError::Request(format!("ssh connect failed: {e}")))?;

        let authenticated = if let Some(path) = &target.key_path {
            let key = russh::keys::load_secret_key(path, None)
                .map_err(|e| NodeError::Other(format!("could not load {path}: {e}")))?;
            session
                .authenticate_publickey(
                    &target.user,
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
                )
                .await
                .map_err(|e| NodeError::Request(format!("ssh key auth failed: {e}")))?
        } else if let Some(password) = &target.password {
            session
                .authenticate_password(&target.user, password)
                .await
                .map_err(|e| NodeError::Request(format!("ssh password auth failed: {e}")))?
        } else {
            return Err(NodeError::Other(
                "no ssh credential configured: set HAJIME_SSH_KEY or HAJIME_SSH_PASSWORD"
                    .into(),
            ));
        };

        if !authenticated.success() {
            return Err(NodeError::Request("ssh authentication rejected".into()));
        }

        let mut channel = session
            .channel_open_session()
            .await
            .map_err(|e| NodeError::Request(format!("ssh channel failed: {e}")))?;
        channel
            .exec(true, command)
            .await
            .map_err(|e| NodeError::Request(format!("ssh exec failed: {e}")))?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut code: i32 = -1;

        while let Some(msg) = channel.wait().await {
            match msg {
                russh::ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                russh::ChannelMsg::ExtendedData { ref data, .. } => {
                    stderr.extend_from_slice(data)
                }
                russh::ChannelMsg::ExitStatus { exit_status } => code = exit_status as i32,
                russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                _ => {}
            }
            if stdout.len() + stderr.len() > self.policy.max_bytes {
                break;
            }
        }

        Ok(Item::from_json(serde_json::json!({
            "stdout": String::from_utf8_lossy(&stdout),
            "stderr": String::from_utf8_lossy(&stderr),
            "exitCode": code,
        })))
    }
}

#[async_trait]
impl NodeExecutor for Ssh {
    /// Runs a command on another machine.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::External
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        if !self.policy.allow_ssh {
            return Err(NodeError::Other(
                "ssh is disabled. Set HAJIME_ALLOW_SSH=1 to enable it, and only \
                 once you are satisfied no exposed webhook can reach this node."
                    .into(),
            ));
        }
        let Some(target) = &self.target else {
            return Err(NodeError::Other(
                "ssh is enabled but not configured. Set HAJIME_SSH_HOST and \
                 HAJIME_SSH_FINGERPRINT; credentials are never read from the \
                 workflow file."
                    .into(),
            ));
        };

        let items = if input.is_empty() { vec![Item::empty()] } else { input };
        let mut out = Vec::with_capacity(items.len());
        for item in &items {
            let command = Self::command_for(node, item)?;
            out.push(self.run_one(target, &command).await?);
        }
        Ok(NodeOutput::items(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(command: &str) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "SSH",
            "type": "n8n-nodes-base.ssh",
            "parameters": {"authentication": "password", "command": command},
        }))
        .unwrap()
    }

    fn target() -> SshTarget {
        SshTarget {
            host: "127.0.0.1".into(),
            port: 22,
            user: "root".into(),
            password: Some("x".into()),
            key_path: None,
            fingerprint: "SHA256:abc".into(),
        }
    }

    #[tokio::test]
    async fn refused_unless_explicitly_enabled() {
        let err = Ssh::new(Policy::default(), Some(target()))
            .execute(&node("uname -a"), vec![], &ExecContext::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("HAJIME_ALLOW_SSH"), "got: {err}");
    }

    #[tokio::test]
    async fn enabled_but_unconfigured_says_so_clearly() {
        let err = Ssh::new(Policy::permissive(vec![]), None)
            .execute(&node("uname -a"), vec![], &ExecContext::default())
            .await
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("HAJIME_SSH_FINGERPRINT"), "got: {text}");
        assert!(text.contains("never read from the workflow file"), "got: {text}");
    }

    #[test]
    fn resolves_the_webhook_body_expression_used_by_ssh_exec() {
        let n = node("={{ $json.body.cmd }}");
        let item = Item::from_json(serde_json::json!({"body": {"cmd": "df -h"}}));
        assert_eq!(Ssh::command_for(&n, &item).unwrap(), "df -h");
    }

    #[test]
    fn an_empty_command_is_refused() {
        let n = node("={{ $json.body.cmd }}");
        let item = Item::from_json(serde_json::json!({"body": {"cmd": ""}}));
        assert!(Ssh::command_for(&n, &item).is_err());
    }

    #[test]
    fn a_target_without_a_fingerprint_cannot_be_built_from_env() {
        // Absent HAJIME_SSH_FINGERPRINT the target is None, so the node refuses
        // rather than trusting whatever key answers.
        temp_env_absent();
        assert!(SshTarget::from_env().is_none());
    }

    fn temp_env_absent() {
        std::env::remove_var("HAJIME_SSH_HOST");
        std::env::remove_var("HAJIME_SSH_FINGERPRINT");
    }
}
