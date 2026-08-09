//! Workflow data model, wire-compatible with n8n's exported JSON.
//!
//! Field names and shapes mirror n8n so an existing workflow export can be
//! loaded without a conversion step. Node type strings keep the
//! `n8n-nodes-base.` prefix on the wire and are normalised on read.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The unit of data that flows between nodes. n8n passes a list of these on
/// every connection, and a node runs once per input list, not once per item.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Item {
    #[serde(default)]
    pub json: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<HashMap<String, BinaryData>>,
}

impl Item {
    pub fn from_json(json: serde_json::Value) -> Self {
        Self { json, binary: None }
    }

    /// An empty item, which is what triggers emit when they carry no payload.
    pub fn empty() -> Self {
        Self::from_json(serde_json::json!({}))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BinaryData {
    pub data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub name: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub parameters: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "typeVersion", default)]
    pub type_version: f64,
    #[serde(default)]
    pub position: Vec<f64>,
    /// A disabled node is skipped and passes its input straight through.
    #[serde(default)]
    pub disabled: bool,
}

impl Node {
    /// `n8n-nodes-base.httpRequest` becomes `httpRequest`. Types without the
    /// prefix are returned unchanged so native Hajime nodes can coexist.
    pub fn kind(&self) -> &str {
        self.node_type
            .rsplit_once('.')
            .map(|(_, k)| k)
            .unwrap_or(&self.node_type)
    }

    pub fn param(&self, key: &str) -> Option<&serde_json::Value> {
        self.parameters.get(key)
    }

    pub fn param_str(&self, key: &str) -> Option<&str> {
        self.param(key).and_then(|v| v.as_str())
    }
}

/// One edge in the graph. n8n keys these by source node name, then by output
/// kind, then by output index, hence the nested arrays in [`Connections`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Connection {
    pub node: String,
    #[serde(rename = "type", default = "default_main")]
    pub conn_type: String,
    #[serde(default)]
    pub index: usize,
}

fn default_main() -> String {
    "main".to_string()
}

/// `{ "Source Node": { "main": [ [ {node: "Target"} ] ] } }`
///
/// The outer array is indexed by output port, the inner array lists every
/// target fed by that port.
pub type Connections = HashMap<String, HashMap<String, Vec<Vec<Connection>>>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub connections: Connections,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub settings: serde_json::Value,
}

impl Workflow {
    pub fn node(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// Targets fed by `name`'s main output at `port`.
    pub fn targets(&self, name: &str, port: usize) -> Vec<&Connection> {
        self.connections
            .get(name)
            .and_then(|by_type| by_type.get("main"))
            .and_then(|ports| ports.get(port))
            .map(|targets| targets.iter().collect())
            .unwrap_or_default()
    }

    /// Every node that feeds `name`, in declaration order.
    pub fn sources(&self, name: &str) -> Vec<&str> {
        let mut out = Vec::new();
        for (source, by_type) in &self.connections {
            for ports in by_type.values() {
                for targets in ports {
                    if targets.iter().any(|c| c.node == name) {
                        out.push(source.as_str());
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Nodes that start an execution. A workflow with none cannot run.
    pub fn triggers(&self) -> Vec<&Node> {
        self.nodes
            .iter()
            .filter(|n| !n.disabled && is_trigger(n.kind()))
            .collect()
    }
}

/// Trigger kinds start a run rather than transforming data. Kept as a function
/// rather than a field so imported workflows need no annotation.
pub fn is_trigger(kind: &str) -> bool {
    matches!(
        kind,
        "webhook"
            | "scheduleTrigger"
            | "cron"
            | "manualTrigger"
            | "executeWorkflowTrigger"
            | "errorTrigger"
            | "intervalTrigger"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real export: schedule trigger into an HTTP request.
    const SAMPLE: &str = r#"{
      "name": "Health Monitor",
      "nodes": [
        {"name":"Every Hour","type":"n8n-nodes-base.scheduleTrigger",
         "typeVersion":1.2,"position":[0,0],"parameters":{}},
        {"name":"Check","type":"n8n-nodes-base.httpRequest",
         "typeVersion":4.2,"position":[200,0],
         "parameters":{"url":"https://example.test/health"}}
      ],
      "connections": {
        "Every Hour": {"main": [[{"node":"Check","type":"main","index":0}]]}
      },
      "active": true
    }"#;

    #[test]
    fn parses_an_n8n_export_without_conversion() {
        let wf: Workflow = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(wf.name, "Health Monitor");
        assert_eq!(wf.nodes.len(), 2);
        assert!(wf.active);
    }

    #[test]
    fn strips_the_nodes_base_prefix() {
        let wf: Workflow = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(wf.node("Check").unwrap().kind(), "httpRequest");
        assert_eq!(wf.node("Every Hour").unwrap().kind(), "scheduleTrigger");
    }

    #[test]
    fn resolves_edges_in_both_directions() {
        let wf: Workflow = serde_json::from_str(SAMPLE).unwrap();
        let targets = wf.targets("Every Hour", 0);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].node, "Check");
        assert_eq!(wf.sources("Check"), vec!["Every Hour"]);
        assert!(wf.targets("Check", 0).is_empty());
    }

    #[test]
    fn finds_the_trigger() {
        let wf: Workflow = serde_json::from_str(SAMPLE).unwrap();
        let triggers = wf.triggers();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].name, "Every Hour");
    }

    #[test]
    fn reads_node_parameters() {
        let wf: Workflow = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(
            wf.node("Check").unwrap().param_str("url"),
            Some("https://example.test/health")
        );
    }
}
