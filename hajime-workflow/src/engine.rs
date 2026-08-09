//! Graph execution.
//!
//! Follows n8n's model: a run starts at one trigger, each node consumes the
//! items produced by its inputs, and a node runs once per input list rather
//! than once per item. A node only runs after every one of its sources has
//! produced data, which keeps merge points correct without a scheduler.

use crate::model::{Item, Node, Workflow};
use crate::nodes::{Effect, ExecContext, NodeError, NodeOutput, Registry};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeRun {
    pub node: String,
    pub kind: String,
    pub items_out: usize,
    pub duration_us: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RunResult {
    pub workflow: String,
    pub success: bool,
    pub runs: Vec<NodeRun>,
    pub duration_us: u128,
    /// Items held by each node after the run, for inspection and for
    /// `respondToWebhook` to find its payload.
    #[serde(skip)]
    pub outputs: HashMap<String, Vec<Item>>,
}

impl RunResult {
    pub fn last_items(&self) -> Option<&Vec<Item>> {
        self.runs
            .last()
            .and_then(|r| self.outputs.get(&r.node))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("workflow has no enabled trigger node")]
    NoTrigger,
    #[error("trigger '{0}' not found in workflow")]
    UnknownTrigger(String),
    #[error("node '{node}' failed: {source}")]
    Node {
        node: String,
        #[source]
        source: NodeError,
    },
}

/// Whether a run is allowed to change anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Nodes do what they say.
    Live,
    /// Nodes that would change something outside this process are held back.
    ///
    /// The graph is still walked, the reads still happen and the expressions
    /// are still evaluated against real data, so the transcript shows what the
    /// workflow would actually have done. This is what makes an imported
    /// workflow safe to try before it is trusted with live credentials.
    Shadow,
}

impl Mode {
    /// Would this effect be held back?
    ///
    /// Stricter than the tool gateway, which stops only `External`. A workflow
    /// rehearsal that overwrites a file on disk is not a rehearsal, and unlike
    /// the gateway's callers a workflow node is often chosen precisely because
    /// it writes something.
    pub fn blocks(&self, effect: Effect) -> bool {
        matches!(self, Mode::Shadow) && effect != Effect::Read
    }
}

pub struct Engine {
    registry: Registry,
    /// Guards against a cycle burning the executor. n8n has no cycles in the
    /// main graph, so hitting this means the workflow is malformed.
    max_steps: usize,
    mode: Mode,
}

impl Engine {
    pub fn new(registry: Registry) -> Self {
        Self { registry, max_steps: 1000, mode: Mode::Live }
    }

    /// An engine that walks the graph without changing anything.
    pub fn shadow(registry: Registry) -> Self {
        Self { registry, max_steps: 1000, mode: Mode::Shadow }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Run `workflow`, starting at `trigger` (or its only trigger when `None`).
    /// `payload` seeds the trigger's output, which is how a webhook body or a
    /// parent workflow's items enter the graph.
    pub async fn run(
        &self,
        workflow: &Workflow,
        trigger: Option<&str>,
        payload: Vec<Item>,
    ) -> Result<RunResult, EngineError> {
        let started = Instant::now();

        let start = match trigger {
            Some(name) => workflow
                .node(name)
                .ok_or_else(|| EngineError::UnknownTrigger(name.to_string()))?,
            None => *workflow.triggers().first().ok_or(EngineError::NoTrigger)?,
        };

        let seed = if payload.is_empty() { vec![Item::empty()] } else { payload };

        let reachable = Self::reachable_from(workflow, &start.name);

        let mut outputs: HashMap<String, Vec<Item>> = HashMap::new();
        let mut runs: Vec<NodeRun> = Vec::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        outputs.insert(start.name.clone(), seed);
        done.insert(start.name.clone());
        runs.push(NodeRun {
            node: start.name.clone(),
            kind: start.kind().to_string(),
            items_out: outputs[&start.name].len(),
            duration_us: 0,
            error: None,
        });
        self.enqueue_targets(workflow, &start.name, &mut queue);

        let mut steps = 0usize;
        while let Some(name) = queue.pop_front() {
            steps += 1;
            if steps > self.max_steps {
                break;
            }
            if done.contains(&name) {
                continue;
            }
            let Some(node) = workflow.node(&name) else { continue };

            // Wait until every source has run, but only count sources this run
            // can actually reach. A workflow may carry several triggers as
            // alternative entry points; the ones that did not start this run
            // never produce anything, and waiting on them would stall the node
            // forever. Re-queueing is cheap and avoids a topological sort.
            let sources: Vec<&str> = workflow
                .sources(&name)
                .into_iter()
                .filter(|s| reachable.contains(*s))
                .collect();
            if !sources.iter().all(|s| done.contains(*s)) {
                queue.push_back(name);
                continue;
            }

            let input: Vec<Item> = sources
                .iter()
                .filter_map(|s| outputs.get(*s))
                .flat_map(|items| items.iter().cloned())
                .collect();

            if node.disabled {
                outputs.insert(name.clone(), input);
                done.insert(name.clone());
                self.enqueue_targets(workflow, &name, &mut queue);
                continue;
            }

            let step = Instant::now();
            let ctx = ExecContext::new(&outputs);
            let result = self.execute_node(node, input, &ctx).await;
            let elapsed = step.elapsed().as_micros();

            match result {
                Ok(NodeOutput { items }) => {
                    runs.push(NodeRun {
                        node: name.clone(),
                        kind: node.kind().to_string(),
                        items_out: items.len(),
                        duration_us: elapsed,
                        error: None,
                    });
                    outputs.insert(name.clone(), items);
                    done.insert(name.clone());
                    self.enqueue_targets(workflow, &name, &mut queue);
                }
                Err(err) => {
                    runs.push(NodeRun {
                        node: name.clone(),
                        kind: node.kind().to_string(),
                        items_out: 0,
                        duration_us: elapsed,
                        error: Some(err.to_string()),
                    });
                    return Ok(RunResult {
                        workflow: workflow.name.clone(),
                        success: false,
                        runs,
                        duration_us: started.elapsed().as_micros(),
                        outputs,
                    });
                }
            }
        }

        Ok(RunResult {
            workflow: workflow.name.clone(),
            success: true,
            runs,
            duration_us: started.elapsed().as_micros(),
            outputs,
        })
    }

    async fn execute_node(
        &self,
        node: &Node,
        input: Vec<Item>,
        ctx: &ExecContext<'_>,
    ) -> Result<NodeOutput, NodeError> {
        match self.registry.get(node.kind()) {
            Some(exec) => {
                let effect = exec.effect(node);
                if self.mode.blocks(effect) {
                    // Held back, and it says so in the data rather than only in
                    // a log. The item flows on to the next node, so the rest of
                    // the graph still runs and the transcript stays complete.
                    return Ok(NodeOutput::single(serde_json::json!({
                        "shadow": true,
                        "wouldHaveRun": node.name,
                        "kind": node.kind(),
                        "effect": effect,
                        "itemsIn": input.len(),
                        "note": "not executed: this node changes something \
                                 outside the process and the run is in shadow mode",
                    })));
                }
                exec.execute(node, input, ctx).await
            }
            // An unimplemented node passes data through rather than aborting
            // the run, so a partially ported workflow still reaches its end.
            None => Ok(NodeOutput { items: input }),
        }
    }

    /// Every node downstream of `start`, including `start` itself.
    fn reachable_from(workflow: &Workflow, start: &str) -> HashSet<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![start.to_string()];
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            for port in 0..4 {
                for conn in workflow.targets(&name, port) {
                    if !seen.contains(&conn.node) {
                        stack.push(conn.node.clone());
                    }
                }
            }
        }
        seen
    }

    fn enqueue_targets(
        &self,
        workflow: &Workflow,
        from: &str,
        queue: &mut VecDeque<String>,
    ) {
        for port in 0..4 {
            for conn in workflow.targets(from, port) {
                if !queue.contains(&conn.node) {
                    queue.push_back(conn.node.clone());
                }
            }
        }
    }
}

/// Turn a run into a history record.
///
/// Lives here rather than in `hajime-core` because `RunResult` is a workflow
/// concept, and the core crate deliberately knows nothing about workflows.
pub fn record_of(
    workflow_id: &str,
    trigger: &str,
    started_at: chrono::DateTime<chrono::Utc>,
    result: &RunResult,
) -> hajime_core::history::Record {
    let failed = result.runs.iter().find(|r| r.error.is_some());
    let duration_ms = (result.duration_us / 1000) as u64;

    if result.success {
        hajime_core::history::Record::success(
            workflow_id,
            &result.workflow,
            trigger,
            started_at,
            duration_ms,
            result.runs.len(),
        )
    } else {
        hajime_core::history::Record::failure(
            workflow_id,
            &result.workflow,
            trigger,
            started_at,
            duration_ms,
            result.runs.len(),
            failed.map(|r| r.node.clone()),
            failed.and_then(|r| r.error.clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::Registry;

    fn linear() -> Workflow {
        serde_json::from_str(
            r#"{
              "name":"linear","nodes":[
                {"name":"T","type":"n8n-nodes-base.manualTrigger","parameters":{}},
                {"name":"A","type":"n8n-nodes-base.noOp","parameters":{}},
                {"name":"B","type":"n8n-nodes-base.noOp","parameters":{}}
              ],
              "connections":{
                "T":{"main":[[{"node":"A","type":"main","index":0}]]},
                "A":{"main":[[{"node":"B","type":"main","index":0}]]}
              }}"#,
        )
        .unwrap()
    }

    /// A workflow whose middle node posts somewhere real.
    fn posting() -> Workflow {
        serde_json::from_str(
            r#"{
              "name":"posting","nodes":[
                {"name":"T","type":"n8n-nodes-base.manualTrigger","parameters":{}},
                {"name":"Post","type":"n8n-nodes-base.httpRequest",
                 "parameters":{"url":"https://example.test/send","method":"POST"}},
                {"name":"After","type":"n8n-nodes-base.noOp","parameters":{}}
              ],
              "connections":{
                "T":{"main":[[{"node":"Post","type":"main","index":0}]]},
                "Post":{"main":[[{"node":"After","type":"main","index":0}]]}
              }}"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_shadow_run_holds_back_a_post_and_says_so() {
        // The node is never executed, so no request leaves the machine. If it
        // had run, the URL does not resolve and the run would have failed.
        let engine = Engine::shadow(Registry::with_builtins());
        let result = engine.run(&posting(), None, vec![]).await.unwrap();

        assert!(result.success, "the graph should still complete");
        let items = &result.outputs["Post"];
        assert_eq!(items.len(), 1);
        let json = &items[0].json;
        assert_eq!(json["shadow"], serde_json::json!(true));
        assert_eq!(json["wouldHaveRun"], serde_json::json!("Post"));
        assert_eq!(json["effect"], serde_json::json!("external"));
    }

    #[tokio::test]
    async fn a_shadow_run_still_reaches_the_end_of_the_graph() {
        // Stopping the run at the held-back node would hide everything after
        // it, which is most of what a rehearsal is for.
        let engine = Engine::shadow(Registry::with_builtins());
        let result = engine.run(&posting(), None, vec![]).await.unwrap();
        let visited: Vec<&str> = result.runs.iter().map(|r| r.node.as_str()).collect();
        assert_eq!(visited, vec!["T", "Post", "After"]);
    }

    #[test]
    fn shadow_blocks_by_effect_and_live_blocks_nothing() {
        assert!(!Mode::Live.blocks(Effect::External));
        assert!(!Mode::Live.blocks(Effect::Local));

        assert!(Mode::Shadow.blocks(Effect::External));
        // Stricter than the tool gateway on purpose: a rehearsal that writes a
        // file has changed the machine.
        assert!(Mode::Shadow.blocks(Effect::Local));
        assert!(!Mode::Shadow.blocks(Effect::Read), "reads make the rehearsal realistic");
    }

    #[tokio::test]
    async fn a_get_is_not_held_back_because_it_only_reads() {
        // The effect is read from the node's parameters, not from its kind.
        let reg = Registry::with_builtins();
        let get: Node = serde_json::from_str(
            r#"{"name":"G","type":"n8n-nodes-base.httpRequest",
                "parameters":{"url":"https://example.test","method":"GET"}}"#,
        )
        .unwrap();
        let post: Node = serde_json::from_str(
            r#"{"name":"P","type":"n8n-nodes-base.httpRequest",
                "parameters":{"url":"https://example.test","method":"POST"}}"#,
        )
        .unwrap();
        let exec = reg.get("httpRequest").unwrap();
        assert_eq!(exec.effect(&get), Effect::Read);
        assert_eq!(exec.effect(&post), Effect::External);
    }

    #[tokio::test]
    async fn an_unclassified_node_kind_is_held_back_rather_than_let_through() {
        // The trait default. A node kind added later and never classified must
        // fail closed.
        struct Unclassified;
        #[async_trait::async_trait]
        impl crate::nodes::NodeExecutor for Unclassified {
            async fn execute(
                &self,
                _n: &Node,
                _i: Vec<Item>,
                _c: &ExecContext<'_>,
            ) -> Result<NodeOutput, NodeError> {
                panic!("a shadow run must not execute an unclassified node");
            }
        }
        let mut reg = Registry::empty();
        reg.register("noOp", std::sync::Arc::new(Unclassified));

        let engine = Engine::shadow(reg);
        let result = engine.run(&linear(), None, vec![]).await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn walks_the_graph_in_order() {
        let engine = Engine::new(Registry::empty());
        let result = engine.run(&linear(), None, vec![]).await.unwrap();

        assert!(result.success);
        let visited: Vec<&str> = result.runs.iter().map(|r| r.node.as_str()).collect();
        assert_eq!(visited, vec!["T", "A", "B"]);
    }

    #[tokio::test]
    async fn seeds_the_trigger_with_the_payload() {
        let engine = Engine::new(Registry::empty());
        let payload = vec![Item::from_json(serde_json::json!({"hello": "world"}))];
        let result = engine.run(&linear(), None, payload).await.unwrap();

        // With no executors registered every node passes data through, so the
        // payload should still be intact at the far end.
        let out = result.outputs.get("B").unwrap();
        assert_eq!(out[0].json["hello"], "world");
    }

    #[tokio::test]
    async fn refuses_a_workflow_with_no_trigger() {
        let wf: Workflow = serde_json::from_str(
            r#"{"name":"x","nodes":[
                 {"name":"A","type":"n8n-nodes-base.noOp","parameters":{}}
               ],"connections":{}}"#,
        )
        .unwrap();
        let engine = Engine::new(Registry::empty());
        assert!(matches!(
            engine.run(&wf, None, vec![]).await,
            Err(EngineError::NoTrigger)
        ));
    }

    #[tokio::test]
    async fn waits_for_every_source_before_running_a_merge() {
        // T fans out to A and B, both of which feed C. C must run last and see
        // items from both branches.
        let wf: Workflow = serde_json::from_str(
            r#"{"name":"diamond","nodes":[
                 {"name":"T","type":"n8n-nodes-base.manualTrigger","parameters":{}},
                 {"name":"A","type":"n8n-nodes-base.noOp","parameters":{}},
                 {"name":"B","type":"n8n-nodes-base.noOp","parameters":{}},
                 {"name":"C","type":"n8n-nodes-base.noOp","parameters":{}}
               ],
               "connections":{
                 "T":{"main":[[{"node":"A","type":"main","index":0},
                               {"node":"B","type":"main","index":0}]]},
                 "A":{"main":[[{"node":"C","type":"main","index":0}]]},
                 "B":{"main":[[{"node":"C","type":"main","index":0}]]}
               }}"#,
        )
        .unwrap();

        let engine = Engine::new(Registry::empty());
        let result = engine.run(&wf, None, vec![]).await.unwrap();

        let visited: Vec<&str> = result.runs.iter().map(|r| r.node.as_str()).collect();
        assert_eq!(visited.last(), Some(&"C"));
        // One empty item from each branch.
        assert_eq!(result.outputs.get("C").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_second_trigger_does_not_stall_the_run() {
        // Two triggers feeding one node are alternative entry points, not a
        // merge. Waiting for the trigger that did not fire would hang the node.
        let wf: Workflow = serde_json::from_str(
            r#"{"name":"two-triggers","nodes":[
                 {"name":"Manual","type":"n8n-nodes-base.manualTrigger","parameters":{}},
                 {"name":"Hourly","type":"n8n-nodes-base.scheduleTrigger","parameters":{}},
                 {"name":"Work","type":"n8n-nodes-base.noOp","parameters":{}}
               ],
               "connections":{
                 "Manual":{"main":[[{"node":"Work","type":"main","index":0}]]},
                 "Hourly":{"main":[[{"node":"Work","type":"main","index":0}]]}
               }}"#,
        )
        .unwrap();

        let engine = Engine::new(Registry::empty());
        let payload = vec![Item::from_json(serde_json::json!({"from": "manual"}))];
        let result = engine.run(&wf, Some("Manual"), payload).await.unwrap();

        assert!(result.success);
        let out = result.outputs.get("Work").expect("Work should have run");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].json["from"], "manual");
        // The trigger that did not start this run contributes nothing.
        assert!(!result.outputs.contains_key("Hourly"));
    }

    #[tokio::test]
    async fn a_disabled_node_passes_its_input_through() {
        let mut wf = linear();
        wf.nodes.iter_mut().find(|n| n.name == "A").unwrap().disabled = true;

        let engine = Engine::new(Registry::empty());
        let payload = vec![Item::from_json(serde_json::json!({"n": 1}))];
        let result = engine.run(&wf, None, payload).await.unwrap();

        assert!(result.success);
        assert_eq!(result.outputs.get("B").unwrap()[0].json["n"], 1);
        // The disabled node is skipped, so it records no run.
        assert!(!result.runs.iter().any(|r| r.node == "A"));
    }
}
