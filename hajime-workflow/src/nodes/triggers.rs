//! Entry-point nodes.
//!
//! A trigger does not transform data: the engine seeds it with the payload that
//! started the run and moves on. These executors exist to validate the node's
//! configuration and to expose the details the HTTP server and the scheduler
//! need, so a misconfigured trigger fails at load rather than at 3am.

use super::{Effect, ExecContext, NodeError, NodeExecutor, NodeOutput};
use crate::model::{Item, Node};
use async_trait::async_trait;
use croner::Cron;


/// `scheduleTrigger`.
///
/// n8n nests the schedule as
/// `{"rule": {"interval": [{"field": "cronExpression", "expression": "0 * * * *"}]}}`.
/// Most imported workflows use `cronExpression` directly; one active workflow
/// in a real export used the friendlier `hours` form instead, so that
/// conversion is not a hypothetical. `minutes` and `days` are recognised too,
/// on the same n8n-editor pattern, though no imported workflow uses them yet.
#[derive(Default)]
pub struct ScheduleTrigger;

impl ScheduleTrigger {
    /// Every cron expression this node fires on. A node may carry several.
    pub fn expressions(node: &Node) -> Result<Vec<String>, NodeError> {
        let intervals = node
            .param("rule")
            .and_then(|r| r.get("interval"))
            .and_then(|i| i.as_array())
            .ok_or(NodeError::MissingParameter("rule.interval"))?;

        let mut out = Vec::new();
        for entry in intervals {
            let field = entry.get("field").and_then(|f| f.as_str()).unwrap_or("");
            match field {
                "cronExpression" => {
                    let expr = entry
                        .get("expression")
                        .and_then(|e| e.as_str())
                        .ok_or(NodeError::MissingParameter("rule.interval.expression"))?;
                    out.push(expr.to_string());
                }
                "minutes" => {
                    let n = entry.get("minutesInterval").and_then(|v| v.as_u64()).unwrap_or(1);
                    out.push(format!("*/{n} * * * *"));
                }
                "hours" => {
                    let n = entry.get("hoursInterval").and_then(|v| v.as_u64()).unwrap_or(1);
                    out.push(format!("0 */{n} * * *"));
                }
                "days" => {
                    let hour = entry.get("triggerAtHour").and_then(|v| v.as_u64()).unwrap_or(0);
                    let minute = entry.get("triggerAtMinute").and_then(|v| v.as_u64()).unwrap_or(0);
                    out.push(format!("{minute} {hour} * * *"));
                }
                other => {
                    return Err(NodeError::InvalidParameter {
                        name: "rule.interval.field",
                        reason: format!("unsupported schedule field '{other}'"),
                    })
                }
            }
        }
        Ok(out)
    }

    /// Parse and validate, so a bad expression is caught when the workflow is
    /// loaded instead of when it should have fired.
    ///
    /// `Cron::from_str` accepts any string without checking it; only
    /// `Cron::new(..).parse()` validates. Using the former would let a typo
    /// through as a trigger that simply never fires.
    pub fn crons(node: &Node) -> Result<Vec<Cron>, NodeError> {
        Self::expressions(node)?
            .into_iter()
            .map(|e| {
                Cron::new(&e).parse().map_err(|err| NodeError::InvalidParameter {
                    name: "rule.interval.expression",
                    reason: format!("'{e}' is not a valid cron expression: {err}"),
                })
            })
            .collect()
    }
}

#[async_trait]
impl NodeExecutor for ScheduleTrigger {
    /// A trigger only hands the run its starting items.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        Self::crons(node)?;
        Ok(NodeOutput::items(input))
    }
}

/// `webhook`. The HTTP server owns routing; this validates and exposes config.
#[derive(Default)]
pub struct Webhook;

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseMode {
    /// Reply with the last node's items once the run finishes.
    LastNode,
    /// Reply with whatever a `respondToWebhook` node produces.
    ResponseNode,
    /// Reply immediately, then run.
    Immediately,
}

impl Webhook {
    pub fn path(node: &Node) -> Result<String, NodeError> {
        node.param_str("path")
            .map(|p| p.trim_start_matches('/').to_string())
            .filter(|p| !p.is_empty())
            .ok_or(NodeError::MissingParameter("path"))
    }

    pub fn method(node: &Node) -> String {
        node.param_str("httpMethod").unwrap_or("GET").to_uppercase()
    }

    pub fn response_mode(node: &Node) -> ResponseMode {
        match node.param_str("responseMode").unwrap_or("lastNode") {
            "responseNode" => ResponseMode::ResponseNode,
            "onReceived" | "immediately" => ResponseMode::Immediately,
            _ => ResponseMode::LastNode,
        }
    }
}

#[async_trait]
impl NodeExecutor for Webhook {
    /// A trigger only hands the run its starting items.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        Self::path(node)?;
        Ok(NodeOutput::items(input))
    }
}

/// `executeWorkflowTrigger`. Carries no parameters: a parent workflow calls in
/// and its items become this run's payload.
#[derive(Default)]
pub struct ExecuteWorkflowTrigger;

#[async_trait]
impl NodeExecutor for ExecuteWorkflowTrigger {
    /// A trigger only hands the run its starting items.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        _node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        Ok(NodeOutput::items(input))
    }
}

/// `manualTrigger`. Fires only when a run is started by hand.
#[derive(Default)]
pub struct ManualTrigger;

#[async_trait]
impl NodeExecutor for ManualTrigger {
    /// A trigger only hands the run its starting items.
    fn effect(&self, _node: &Node) -> Effect {
        Effect::Read
    }

    async fn execute(
        &self,
        _node: &Node,
        input: Vec<Item>,
        _ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        Ok(NodeOutput::items(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: &str, params: serde_json::Value) -> Node {
        serde_json::from_value(serde_json::json!({
            "name": "T",
            "type": format!("n8n-nodes-base.{kind}"),
            "parameters": params,
        }))
        .unwrap()
    }

    #[test]
    fn reads_the_cron_shape_used_in_production() {
        // Taken verbatim from the exported Hourly Health Monitor workflow.
        let n = node(
            "scheduleTrigger",
            serde_json::json!({"rule": {"interval": [
                {"field": "cronExpression", "expression": "0 * * * *"}
            ]}}),
        );
        assert_eq!(ScheduleTrigger::expressions(&n).unwrap(), vec!["0 * * * *"]);
        assert_eq!(ScheduleTrigger::crons(&n).unwrap().len(), 1);
    }

    #[test]
    fn accepts_every_schedule_in_the_export() {
        for expr in ["30 9 * * 1", "20 3 * * *", "0 * * * *", "12 0 * * *"] {
            let n = node(
                "scheduleTrigger",
                serde_json::json!({"rule": {"interval": [
                    {"field": "cronExpression", "expression": expr}
                ]}}),
            );
            assert!(ScheduleTrigger::crons(&n).is_ok(), "{expr} should parse");
        }
    }

    #[test]
    fn converts_the_friendly_interval_forms() {
        let mins = node(
            "scheduleTrigger",
            serde_json::json!({"rule": {"interval": [
                {"field": "minutes", "minutesInterval": 15}
            ]}}),
        );
        assert_eq!(ScheduleTrigger::expressions(&mins).unwrap(), vec!["*/15 * * * *"]);

        let daily = node(
            "scheduleTrigger",
            serde_json::json!({"rule": {"interval": [
                {"field": "days", "triggerAtHour": 3, "triggerAtMinute": 20}
            ]}}),
        );
        assert_eq!(ScheduleTrigger::expressions(&daily).unwrap(), vec!["20 3 * * *"]);
    }

    #[test]
    fn the_hours_form_used_by_an_imported_workflow_converts_correctly() {
        // The only active workflow in a real n8n export whose schedule is
        // not written as a raw cronExpression. `minutes` and `days` had unit
        // coverage; this branch of the same match arm did not, and it is the
        // one the real export actually exercises.
        let n = node(
            "scheduleTrigger",
            serde_json::json!({"rule": {"interval": [
                {"field": "hours", "hoursInterval": 12}
            ]}}),
        );
        assert_eq!(ScheduleTrigger::expressions(&n).unwrap(), vec!["0 */12 * * *"]);
        assert_eq!(ScheduleTrigger::crons(&n).unwrap().len(), 1);
    }

    #[test]
    fn a_bad_cron_fails_at_load_time() {
        let n = node(
            "scheduleTrigger",
            serde_json::json!({"rule": {"interval": [
                {"field": "cronExpression", "expression": "not a cron"}
            ]}}),
        );
        assert!(matches!(
            ScheduleTrigger::crons(&n),
            Err(NodeError::InvalidParameter { name: "rule.interval.expression", .. })
        ));
    }

    #[test]
    fn webhook_reads_path_method_and_mode() {
        let n = node(
            "webhook",
            serde_json::json!({
                "path": "get-totp", "httpMethod": "GET", "responseMode": "responseNode"
            }),
        );
        assert_eq!(Webhook::path(&n).unwrap(), "get-totp");
        assert_eq!(Webhook::method(&n), "GET");
        assert_eq!(Webhook::response_mode(&n), ResponseMode::ResponseNode);
    }

    #[test]
    fn webhook_defaults_match_n8n() {
        let n = node("webhook", serde_json::json!({"path": "/hook"}));
        // A leading slash is stripped so routing keys stay consistent.
        assert_eq!(Webhook::path(&n).unwrap(), "hook");
        assert_eq!(Webhook::method(&n), "GET");
        assert_eq!(Webhook::response_mode(&n), ResponseMode::LastNode);
    }

    #[test]
    fn webhook_without_a_path_is_rejected() {
        let n = node("webhook", serde_json::json!({"httpMethod": "POST"}));
        assert!(matches!(Webhook::path(&n), Err(NodeError::MissingParameter("path"))));
    }
}
