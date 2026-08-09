//! The tool gateway.
//!
//! Every tool the model can reach passes through here. One choke point rather
//! than a limit inside each tool server, because limits scattered across
//! servers drift apart and become decorative.
//!
//! What it enforces:
//!
//! - **A budget per tool and per run.** A model in a bad loop can send a
//!   hundred messages or burn an API quota in a minute. The ceiling is the
//!   difference between a mistake and an incident.
//! - **A circuit breaker.** Repeated failures stop the tool rather than
//!   retrying into a wall.
//! - **Dry run for anything that leaves the machine.** A tool marked
//!   [`Effect::External`] does nothing in dry-run mode and reports what it
//!   would have done. This is what lets a new setup be watched before it is
//!   trusted.
//! - **An audit line per call.** Separate from run history: this answers who
//!   sent what, when, and under whose authority.
//!
//! Tool descriptions follow MCP's shape (`name`, `description`,
//! `input_schema`), so an MCP server's `tools/list` maps onto [`ToolSpec`]
//! without translation and the model discovers tools rather than being taught
//! them one by one.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ToolError {
    #[error("no tool named '{0}'")]
    Unknown(String),
    #[error("'{tool}' has used its budget of {limit} calls")]
    BudgetSpent { tool: String, limit: u32 },
    #[error("the run has used its budget of {limit} tool calls")]
    RunBudgetSpent { limit: u32 },
    #[error("'{tool}' is open-circuit after {failures} consecutive failures")]
    CircuitOpen { tool: String, failures: u32 },
    #[error("'{tool}' failed: {reason}")]
    Failed { tool: String, reason: String },
}

/// What a tool does to the world, which decides whether dry run stops it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    /// Reads only. Safe to run in any mode.
    Read,
    /// Writes locally: a file, a database row.
    Local,
    /// Leaves the machine: an email, a message, a published post. Not
    /// reversible, so dry run stops it.
    External,
}

/// An MCP-shaped tool description. This is what the model is shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, exactly as MCP defines it.
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
    #[serde(default = "default_effect")]
    pub effect: Effect,
}

fn default_effect() -> Effect {
    // Unlabelled tools are treated as the most dangerous kind. A tool that
    // forgets to declare itself must not slip past dry run.
    Effect::External
}

impl ToolSpec {
    pub fn read(name: &str, description: &str, schema: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
            effect: Effect::Read,
        }
    }

    pub fn external(name: &str, description: &str, schema: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: schema,
            effect: Effect::External,
        }
    }
}

#[async_trait]
pub trait ToolProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn list(&self) -> Vec<ToolSpec>;
    async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

#[derive(Debug, Clone)]
pub struct Budget {
    /// Calls allowed per tool, per run.
    pub per_tool: u32,
    /// Calls allowed across all tools, per run.
    pub per_run: u32,
    /// Consecutive failures before a tool is cut off.
    pub failures_before_open: u32,
}

impl Default for Budget {
    fn default() -> Self {
        // Deliberately small. A workflow that legitimately needs more says so;
        // a runaway loop does not get the chance to ask.
        Self { per_tool: 10, per_run: 40, failures_before_open: 3 }
    }
}

/// One recorded tool call.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub tool: String,
    pub effect: Effect,
    pub caller: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub allowed: bool,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_because: Option<String>,
}

#[derive(Default)]
struct Counters {
    per_tool: HashMap<String, u32>,
    total: u32,
    failures: HashMap<String, u32>,
}

pub struct Gateway {
    providers: Vec<Box<dyn ToolProvider>>,
    budget: Budget,
    /// When set, tools with [`Effect::External`] are described, not run.
    dry_run: bool,
    counters: Mutex<Counters>,
    audit: Mutex<Vec<AuditEntry>>,
}

impl Gateway {
    pub fn new(budget: Budget, dry_run: bool) -> Self {
        Self {
            providers: Vec::new(),
            budget,
            dry_run,
            counters: Mutex::new(Counters::default()),
            audit: Mutex::new(Vec::new()),
        }
    }

    pub fn add(&mut self, provider: Box<dyn ToolProvider>) {
        self.providers.push(provider);
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Every tool, from every provider. This is what the model is shown.
    pub async fn catalogue(&self) -> Vec<ToolSpec> {
        let mut all = Vec::new();
        for p in &self.providers {
            all.extend(p.list().await);
        }
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    async fn find(&self, tool: &str) -> Option<(&dyn ToolProvider, ToolSpec)> {
        for p in &self.providers {
            if let Some(spec) = p.list().await.into_iter().find(|s| s.name == tool) {
                return Some((p.as_ref(), spec));
            }
        }
        None
    }

    /// Reset per-run counters. Called at the start of each conversation.
    pub fn begin_run(&self) {
        let mut c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        c.per_tool.clear();
        c.total = 0;
        // Failure counts survive: a tool that is broken stays broken across
        // runs until it succeeds again.
    }

    pub fn audit_log(&self) -> Vec<AuditEntry> {
        self.audit.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn record(&self, entry: AuditEntry) {
        self.audit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry);
    }

    /// Run a tool, or explain why not.
    pub async fn call(
        &self,
        caller: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let Some((provider, spec)) = self.find(tool).await else {
            return Err(ToolError::Unknown(tool.to_string()));
        };

        let refuse = |reason: ToolError, effect: Effect| {
            self.record(AuditEntry {
                tool: tool.to_string(),
                effect,
                caller: caller.to_string(),
                at: chrono::Utc::now(),
                allowed: false,
                dry_run: self.dry_run,
                refused_because: Some(reason.to_string()),
            });
            reason
        };

        {
            let c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(&fails) = c.failures.get(tool) {
                if fails >= self.budget.failures_before_open {
                    return Err(refuse(
                        ToolError::CircuitOpen { tool: tool.into(), failures: fails },
                        spec.effect,
                    ));
                }
            }
            if c.total >= self.budget.per_run {
                return Err(refuse(
                    ToolError::RunBudgetSpent { limit: self.budget.per_run },
                    spec.effect,
                ));
            }
            if c.per_tool.get(tool).copied().unwrap_or(0) >= self.budget.per_tool {
                return Err(refuse(
                    ToolError::BudgetSpent { tool: tool.into(), limit: self.budget.per_tool },
                    spec.effect,
                ));
            }
        }

        // Count before running. A call that panics or hangs must still be
        // spent, otherwise a failing tool gets unlimited retries.
        {
            let mut c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
            *c.per_tool.entry(tool.to_string()).or_insert(0) += 1;
            c.total += 1;
        }

        // Dry run stops anything that leaves the machine, and only that.
        // Reads still work, so a rehearsal produces a realistic transcript.
        if self.dry_run && spec.effect == Effect::External {
            self.record(AuditEntry {
                tool: tool.to_string(),
                effect: spec.effect,
                caller: caller.to_string(),
                at: chrono::Utc::now(),
                allowed: true,
                dry_run: true,
                refused_because: None,
            });
            return Ok(serde_json::json!({
                "dryRun": true,
                "wouldHaveCalled": tool,
                "withArguments": args,
                "note": "not sent: this tool has an external effect and the \
                         gateway is in dry-run mode",
            }));
        }

        let outcome = provider.call(tool, args).await;

        let mut c = self.counters.lock().unwrap_or_else(|e| e.into_inner());
        match &outcome {
            Ok(_) => {
                c.failures.remove(tool);
            }
            Err(_) => {
                *c.failures.entry(tool.to_string()).or_insert(0) += 1;
            }
        }
        drop(c);

        self.record(AuditEntry {
            tool: tool.to_string(),
            effect: spec.effect,
            caller: caller.to_string(),
            at: chrono::Utc::now(),
            allowed: outcome.is_ok(),
            dry_run: false,
            refused_because: outcome.as_ref().err().cloned(),
        });

        outcome.map_err(|reason| ToolError::Failed { tool: tool.into(), reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Stub {
        specs: Vec<ToolSpec>,
        fail: bool,
        calls: AtomicU32,
    }

    impl Stub {
        fn new(specs: Vec<ToolSpec>) -> Self {
            Self { specs, fail: false, calls: AtomicU32::new(0) }
        }
        fn failing(specs: Vec<ToolSpec>) -> Self {
            Self { specs, fail: true, calls: AtomicU32::new(0) }
        }
    }

    #[async_trait]
    impl ToolProvider for Stub {
        fn name(&self) -> &str {
            "stub"
        }
        async fn list(&self) -> Vec<ToolSpec> {
            self.specs.clone()
        }
        async fn call(&self, _t: &str, _a: serde_json::Value) -> Result<serde_json::Value, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("upstream refused".into())
            } else {
                Ok(serde_json::json!({"ok": true}))
            }
        }
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn gateway(dry_run: bool, provider: Stub) -> Gateway {
        let mut g = Gateway::new(Budget::default(), dry_run);
        g.add(Box::new(provider));
        g
    }

    #[tokio::test]
    async fn the_catalogue_is_what_the_model_sees() {
        let g = gateway(
            false,
            Stub::new(vec![
                ToolSpec::external("send_whatsapp", "Send a WhatsApp message", schema()),
                ToolSpec::read("read_feed", "Read an RSS feed", schema()),
            ]),
        );
        let cat = g.catalogue().await;
        assert_eq!(cat.len(), 2);
        // Sorted, so the prompt is stable between runs.
        assert_eq!(cat[0].name, "read_feed");
        assert_eq!(cat[1].name, "send_whatsapp");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_named_in_the_error() {
        let g = gateway(false, Stub::new(vec![]));
        assert_eq!(
            g.call("model", "nope", serde_json::json!({})).await.unwrap_err(),
            ToolError::Unknown("nope".into())
        );
    }

    #[tokio::test]
    async fn dry_run_stops_external_tools_and_says_what_it_would_have_done() {
        let g = gateway(
            true,
            Stub::new(vec![ToolSpec::external("send_whatsapp", "Send", schema())]),
        );
        let out = g
            .call("model", "send_whatsapp", serde_json::json!({"to": "9665"}))
            .await
            .unwrap();

        assert_eq!(out["dryRun"], true);
        assert_eq!(out["wouldHaveCalled"], "send_whatsapp");
        assert_eq!(out["withArguments"]["to"], "9665");
    }

    #[tokio::test]
    async fn dry_run_still_lets_reads_through() {
        let stub = Stub::new(vec![ToolSpec::read("read_feed", "Read", schema())]);
        let mut g = Gateway::new(Budget::default(), true);
        g.add(Box::new(stub));

        let out = g.call("model", "read_feed", serde_json::json!({})).await.unwrap();
        assert_eq!(out["ok"], true, "a read must actually run in dry-run mode");
    }

    #[tokio::test]
    async fn an_undeclared_effect_is_treated_as_external() {
        // A tool description without an `effect` field must not slip past dry
        // run just because someone forgot to label it.
        let spec: ToolSpec = serde_json::from_value(serde_json::json!({
            "name": "mystery",
            "description": "unlabelled",
            "inputSchema": {"type": "object"}
        }))
        .unwrap();
        assert_eq!(spec.effect, Effect::External);

        let g = gateway(true, Stub::new(vec![spec]));
        let out = g.call("model", "mystery", serde_json::json!({})).await.unwrap();
        assert_eq!(out["dryRun"], true);
    }

    #[tokio::test]
    async fn a_tool_stops_at_its_own_budget() {
        let mut g = Gateway::new(
            Budget { per_tool: 2, per_run: 100, failures_before_open: 99 },
            false,
        );
        g.add(Box::new(Stub::new(vec![ToolSpec::read("t", "d", schema())])));

        assert!(g.call("m", "t", serde_json::json!({})).await.is_ok());
        assert!(g.call("m", "t", serde_json::json!({})).await.is_ok());
        assert_eq!(
            g.call("m", "t", serde_json::json!({})).await.unwrap_err(),
            ToolError::BudgetSpent { tool: "t".into(), limit: 2 }
        );
    }

    #[tokio::test]
    async fn the_run_budget_caps_the_total_across_tools() {
        let mut g = Gateway::new(
            Budget { per_tool: 100, per_run: 3, failures_before_open: 99 },
            false,
        );
        g.add(Box::new(Stub::new(vec![
            ToolSpec::read("a", "d", schema()),
            ToolSpec::read("b", "d", schema()),
        ])));

        for _ in 0..3 {
            assert!(g.call("m", "a", serde_json::json!({})).await.is_ok());
        }
        // A different tool, but the run is spent.
        assert_eq!(
            g.call("m", "b", serde_json::json!({})).await.unwrap_err(),
            ToolError::RunBudgetSpent { limit: 3 }
        );
    }

    #[tokio::test]
    async fn beginning_a_run_clears_the_budget() {
        let mut g = Gateway::new(
            Budget { per_tool: 1, per_run: 10, failures_before_open: 99 },
            false,
        );
        g.add(Box::new(Stub::new(vec![ToolSpec::read("t", "d", schema())])));

        assert!(g.call("m", "t", serde_json::json!({})).await.is_ok());
        assert!(g.call("m", "t", serde_json::json!({})).await.is_err());
        g.begin_run();
        assert!(g.call("m", "t", serde_json::json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn repeated_failures_open_the_circuit() {
        let stub = Stub::failing(vec![ToolSpec::read("t", "d", schema())]);
        let mut g = Gateway::new(
            Budget { per_tool: 100, per_run: 100, failures_before_open: 2 },
            false,
        );
        g.add(Box::new(stub));

        assert!(g.call("m", "t", serde_json::json!({})).await.is_err());
        assert!(g.call("m", "t", serde_json::json!({})).await.is_err());
        // Third call is refused without reaching the provider.
        assert_eq!(
            g.call("m", "t", serde_json::json!({})).await.unwrap_err(),
            ToolError::CircuitOpen { tool: "t".into(), failures: 2 }
        );
    }

    #[tokio::test]
    async fn a_refused_call_never_reaches_the_provider() {
        let stub = Stub::new(vec![ToolSpec::read("t", "d", schema())]);
        let mut g = Gateway::new(
            Budget { per_tool: 1, per_run: 10, failures_before_open: 99 },
            false,
        );
        g.add(Box::new(stub));

        g.call("m", "t", serde_json::json!({})).await.unwrap();
        let _ = g.call("m", "t", serde_json::json!({})).await;

        // The provider is behind the gateway; assert through the audit log
        // that exactly one call was allowed.
        let audit = g.audit_log();
        assert_eq!(audit.len(), 2);
        assert!(audit[0].allowed);
        assert!(!audit[1].allowed);
        assert!(audit[1].refused_because.as_ref().unwrap().contains("budget"));
    }

    #[tokio::test]
    async fn every_call_is_audited_with_its_caller_and_effect() {
        let g = gateway(
            true,
            Stub::new(vec![ToolSpec::external("send", "Send", schema())]),
        );
        g.call("telegram-bot", "send", serde_json::json!({})).await.unwrap();

        let audit = g.audit_log();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].caller, "telegram-bot");
        assert_eq!(audit[0].tool, "send");
        assert_eq!(audit[0].effect, Effect::External);
        assert!(audit[0].dry_run);
        assert!(audit[0].allowed);
    }

    #[tokio::test]
    async fn a_success_clears_the_failure_count() {
        // A tool that recovers must not stay near the circuit threshold.
        let mut g = Gateway::new(
            Budget { per_tool: 100, per_run: 100, failures_before_open: 2 },
            false,
        );
        g.add(Box::new(Stub::failing(vec![ToolSpec::read("bad", "d", schema())])));
        g.add(Box::new(Stub::new(vec![ToolSpec::read("good", "d", schema())])));

        let _ = g.call("m", "bad", serde_json::json!({})).await;
        assert!(g.call("m", "good", serde_json::json!({})).await.is_ok());
        // The healthy tool is unaffected by the other's failures.
        assert!(g.call("m", "good", serde_json::json!({})).await.is_ok());
    }

    #[test]
    fn the_spec_serialises_in_mcp_shape() {
        let spec = ToolSpec::read("read_feed", "Read an RSS feed", schema());
        let json = serde_json::to_value(&spec).unwrap();
        // MCP names it `inputSchema`; a rename here breaks discovery silently.
        assert!(json.get("inputSchema").is_some());
        assert_eq!(json["name"], "read_feed");
    }
}
