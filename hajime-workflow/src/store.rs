//! Workflow storage and lookup.
//!
//! Loads n8n's own export format (`n8n export:workflow --all`), which is a
//! plain array of workflow objects. Using that format directly means the same
//! file feeds either engine, so a migration can be rolled back by pointing n8n
//! at the file it already understands.

use crate::model::{Node, Workflow};
use crate::nodes::triggers::{ScheduleTrigger, Webhook};
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not read the export: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("workflow '{0}' is not in the store")]
    Unknown(String),
}

/// A webhook route resolved to the workflow and trigger node that own it.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub workflow_id: String,
    pub node: String,
    pub method: String,
    pub path: String,
}

/// A schedule resolved to the workflow and trigger node that own it.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub workflow_id: String,
    pub node: String,
    pub expression: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Summary {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub nodes: usize,
    /// Node kinds with no executor yet. Those pass data through untouched, so
    /// a workflow listing them is only partially ported.
    pub unsupported: Vec<String>,
}

#[derive(Default)]
pub struct Store {
    workflows: RwLock<HashMap<String, Workflow>>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load an `n8n export:workflow --all` document.
    pub fn from_export(json: &str) -> Result<Self, StoreError> {
        // `n8n export:workflow --all` writes an array; the Download button in
        // the editor writes one workflow as a bare object. Both are things a
        // person ends up holding, and the serde error for the wrong one reads
        // "invalid type: map, expected a sequence", which says nothing about
        // what to do next.
        let list: Vec<Workflow> = match serde_json::from_str::<Vec<Workflow>>(json) {
            Ok(list) => list,
            Err(array_err) => match serde_json::from_str::<Workflow>(json) {
                Ok(single) => vec![single],
                // Report the array error: it is the shape the migration
                // produces, so it is the one that was probably intended.
                Err(_) => return Err(array_err.into()),
            },
        };
        let store = Self::new();
        {
            let mut guard = store.workflows.write().expect("store lock poisoned");
            for (index, mut wf) in list.into_iter().enumerate() {
                // An export without ids still needs stable keys.
                if wf.id.is_empty() {
                    wf.id = format!("wf-{index}");
                }
                guard.insert(wf.id.clone(), wf);
            }
        }
        Ok(store)
    }

    pub fn insert(&self, mut workflow: Workflow) -> String {
        if workflow.id.is_empty() {
            workflow.id = format!("wf-{}", self.len() + 1);
        }
        let id = workflow.id.clone();
        self.workflows
            .write()
            .expect("store lock poisoned")
            .insert(id.clone(), workflow);
        id
    }

    pub fn get(&self, id: &str) -> Option<Workflow> {
        self.workflows
            .read()
            .expect("store lock poisoned")
            .get(id)
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.workflows.read().expect("store lock poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn summaries(&self, known_kinds: &[&str]) -> Vec<Summary> {
        let guard = self.workflows.read().expect("store lock poisoned");
        let mut out: Vec<Summary> = guard
            .values()
            .map(|wf| {
                let mut unsupported: Vec<String> = wf
                    .nodes
                    .iter()
                    .filter(|n| !n.disabled && !known_kinds.contains(&n.kind()))
                    .map(|n| n.kind().to_string())
                    .collect();
                unsupported.sort_unstable();
                unsupported.dedup();
                Summary {
                    id: wf.id.clone(),
                    name: wf.name.clone(),
                    active: wf.active,
                    nodes: wf.nodes.len(),
                    unsupported,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Webhook routes of every active workflow.
    ///
    /// A misconfigured trigger is skipped rather than aborting the scan: one
    /// broken workflow must not take the whole service's routing table with it.
    pub fn routes(&self) -> Vec<Route> {
        self.scan_active(|wf, node| {
            if node.kind() != "webhook" {
                return None;
            }
            let path = Webhook::path(node).ok()?;
            Some(Route {
                workflow_id: wf.id.clone(),
                node: node.name.clone(),
                method: Webhook::method(node),
                path,
            })
        })
    }

    /// Cron schedules of every active workflow.
    pub fn schedules(&self) -> Vec<Schedule> {
        let guard = self.workflows.read().expect("store lock poisoned");
        let mut out = Vec::new();
        for wf in guard.values().filter(|w| w.active) {
            for node in wf.nodes.iter().filter(|n| !n.disabled) {
                if node.kind() != "scheduleTrigger" {
                    continue;
                }
                // Validate here so a bad expression is reported at load time
                // rather than as a trigger that quietly never fires.
                if ScheduleTrigger::crons(node).is_err() {
                    continue;
                }
                for expression in ScheduleTrigger::expressions(node).unwrap_or_default() {
                    out.push(Schedule {
                        workflow_id: wf.id.clone(),
                        node: node.name.clone(),
                        expression,
                    });
                }
            }
        }
        out.sort_by_key(|a| (a.workflow_id.clone(), a.node.clone()));
        out
    }

    /// Schedules that were rejected, with the reason, for the status endpoint.
    pub fn broken_schedules(&self) -> Vec<(String, String, String)> {
        let guard = self.workflows.read().expect("store lock poisoned");
        let mut out = Vec::new();
        for wf in guard.values().filter(|w| w.active) {
            for node in wf.nodes.iter().filter(|n| !n.disabled) {
                if node.kind() != "scheduleTrigger" {
                    continue;
                }
                if let Err(e) = ScheduleTrigger::crons(node) {
                    out.push((wf.name.clone(), node.name.clone(), e.to_string()));
                }
            }
        }
        out
    }

    fn scan_active<T, F>(&self, mut pick: F) -> Vec<T>
    where
        F: FnMut(&Workflow, &Node) -> Option<T>,
    {
        let guard = self.workflows.read().expect("store lock poisoned");
        let mut out = Vec::new();
        for wf in guard.values().filter(|w| w.active) {
            for node in wf.nodes.iter().filter(|n| !n.disabled) {
                if let Some(found) = pick(wf, node) {
                    out.push(found);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"[
      {"id":"a1","name":"Hooked","active":true,
       "nodes":[
         {"name":"In","type":"n8n-nodes-base.webhook",
          "parameters":{"path":"get-totp","httpMethod":"GET"}},
         {"name":"Out","type":"n8n-nodes-base.respondToWebhook","parameters":{}}
       ],"connections":{}},
      {"id":"b2","name":"Hourly","active":true,
       "nodes":[
         {"name":"Tick","type":"n8n-nodes-base.scheduleTrigger",
          "parameters":{"rule":{"interval":[{"field":"cronExpression","expression":"0 * * * *"}]}}}
       ],"connections":{}},
      {"id":"c3","name":"Sleeping","active":false,
       "nodes":[
         {"name":"In","type":"n8n-nodes-base.webhook",
          "parameters":{"path":"never","httpMethod":"POST"}}
       ],"connections":{}},
      {"id":"d4","name":"Broken","active":true,
       "nodes":[
         {"name":"Tick","type":"n8n-nodes-base.scheduleTrigger",
          "parameters":{"rule":{"interval":[{"field":"cronExpression","expression":"nonsense"}]}}}
       ],"connections":{}}
    ]"#;

    fn store() -> Store {
        Store::from_export(EXPORT).unwrap()
    }

    #[test]
    fn loads_the_n8n_export_array() {
        let s = store();
        assert_eq!(s.len(), 4);
        assert_eq!(s.get("a1").unwrap().name, "Hooked");
        assert!(s.get("nope").is_none());
    }

    #[test]
    fn a_single_workflow_object_loads_as_well_as_an_array() {
        // The editor's Download button produces this shape. Refusing it sends
        // someone to re-export at the worst possible moment, during a restore.
        let one = r#"{"id":"solo","name":"Solo","active":true,"nodes":[],"connections":{}}"#;
        let s = Store::from_export(one).expect("a bare workflow object should load");
        assert_eq!(s.len(), 1);
        assert_eq!(s.get("solo").unwrap().name, "Solo");
    }

    #[test]
    fn malformed_json_still_reports_the_array_error() {
        // Neither shape parses, so the message names the one the migration
        // actually produces rather than the fallback.
        match Store::from_export("{ this is not json") {
            Ok(_) => panic!("malformed json must not load"),
            Err(e) => assert!(format!("{e}").contains("could not read the export"), "{e}"),
        }
    }

    #[test]
    fn routes_cover_active_workflows_only() {
        let routes = store().routes();
        assert_eq!(routes.len(), 1, "the inactive workflow must not be routed");
        assert_eq!(routes[0].path, "get-totp");
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].workflow_id, "a1");
        assert_eq!(routes[0].node, "In");
    }

    #[test]
    fn schedules_skip_expressions_that_do_not_parse() {
        let schedules = store().schedules();
        assert_eq!(schedules.len(), 1, "the broken cron must not be scheduled");
        assert_eq!(schedules[0].expression, "0 * * * *");
        assert_eq!(schedules[0].workflow_id, "b2");
    }

    #[test]
    fn a_broken_schedule_is_reported_rather_than_hidden() {
        let broken = store().broken_schedules();
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].0, "Broken");
        assert!(broken[0].2.contains("nonsense"), "reason: {}", broken[0].2);
    }

    #[test]
    fn summaries_flag_node_kinds_with_no_executor() {
        let known = ["webhook", "respondToWebhook", "scheduleTrigger"];
        let summaries = store().summaries(&known);
        let hooked = summaries.iter().find(|s| s.id == "a1").unwrap();
        assert!(hooked.unsupported.is_empty());

        let narrow = ["scheduleTrigger"];
        let summaries = store().summaries(&narrow);
        let hooked = summaries.iter().find(|s| s.id == "a1").unwrap();
        assert_eq!(hooked.unsupported, vec!["respondToWebhook", "webhook"]);
    }

    #[test]
    fn an_export_without_ids_still_gets_stable_keys() {
        let s = Store::from_export(
            r#"[{"name":"X","nodes":[],"connections":{}}]"#,
        )
        .unwrap();
        assert_eq!(s.len(), 1);
        assert!(s.get("wf-0").is_some());
    }

    #[test]
    fn a_malformed_export_is_an_error() {
        assert!(Store::from_export("not json").is_err());
    }
}
