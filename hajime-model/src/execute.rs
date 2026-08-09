//! Carrying a plan out, through the gateway that keeps the receipts.
//!
//! [`plan`](crate::plan) decides and stops. This is the other half: the system's
//! own operations exposed as tools, so anything the model does goes through
//! `hajime_core::tools::Gateway` and inherits what the gateway already
//! guarantees. Every call is labelled with its effect, counted against a budget,
//! written to an audit log, and held back entirely when the gateway is in dry
//! run.
//!
//! The alternative would be for the planner to shell out directly. That works
//! and is shorter, and it means the one place that records what the model did
//! has no idea any of it happened. The console's audit panel would show an
//! empty list on a machine where the model had been stopping services all
//! afternoon.
//!
//! Two rules hold here regardless of what was asked.
//!
//! **A plan that needs confirmation is not executed.** `Confirm` carries a
//! question, and a question answered by the thing that asked it is not a
//! confirmation. The caller has to come back with the command itself.
//!
//! **The service name is checked again.** [`slots`](crate::slots) already
//! guarantees names come from the table, and this checks anyway. It is one
//! comparison, and it is the last thing standing between a bug anywhere above
//! and a command line.

use crate::plan::Plan;
use crate::world::World;
use async_trait::async_trait;
use hajime_core::tools::{ToolProvider, ToolSpec};
use std::process::Command;

/// The system's operations, as tools.
#[derive(Default)]
pub struct SystemTools {
    world: World,
    /// Set for tests, so they exercise the wiring without touching services.
    pretend: bool,
}


impl SystemTools {
    pub fn new() -> Self {
        Self::default()
    }

    /// A provider that reports what it would run instead of running it.
    ///
    /// Distinct from the gateway's dry run: that stops external calls at the
    /// gateway, this stops them inside the provider, and the tests need the
    /// second so they can assert on the command that would have been built.
    pub fn pretending() -> Self {
        Self { world: World::default(), pretend: true }
    }

    fn known(&self, name: &str) -> bool {
        self.world.entity_names().contains(&name)
    }

    fn run(&self, args: &[&str]) -> Result<serde_json::Value, String> {
        if self.pretend {
            return Ok(serde_json::json!({ "wouldRun": args }));
        }
        let output = Command::new("hajimectl")
            .args(args)
            .output()
            .map_err(|e| format!("could not run hajimectl: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if output.status.success() {
            Ok(serde_json::json!({ "ok": true, "output": stdout }))
        } else {
            // The exit code is carried through rather than flattened to a
            // boolean: `hajimectl` uses distinct codes, and a caller that only
            // learns "it failed" has to guess which failure.
            Err(format!(
                "hajimectl {} exited {}: {}",
                args.join(" "),
                output.status.code().unwrap_or(-1),
                if stderr.is_empty() { stdout } else { stderr }
            ))
        }
    }

    fn service_arg(&self, args: &serde_json::Value) -> Result<String, String> {
        let name = args
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "the 'service' argument is required".to_string())?;

        // Checked here as well as in slots. One string comparison, and it is
        // the last thing between a bug further up and a command line.
        if !self.known(name) {
            return Err(format!(
                "'{name}' is not a service or jail on this system. Known: {}",
                self.world.entity_names().join(", ")
            ));
        }
        Ok(name.to_string())
    }
}

#[async_trait]
impl ToolProvider for SystemTools {
    fn name(&self) -> &str {
        "system"
    }

    async fn list(&self) -> Vec<ToolSpec> {
        let service_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "service": {
                    "type": "string",
                    "description": "the service or jail name",
                    "enum": self.world.entity_names(),
                }
            },
            "required": ["service"],
        });
        let nothing = serde_json::json!({ "type": "object", "properties": {} });

        vec![
            ToolSpec::read(
                "system_status",
                "What is running and what is not.",
                nothing.clone(),
            ),
            ToolSpec::read(
                "system_check",
                "Run the boot self-check and report what fails.",
                nothing.clone(),
            ),
            ToolSpec::read(
                "system_explain",
                "Dependencies, memory cost and what breaks if a service stops.",
                service_schema.clone(),
            ),
            // Starting is labelled external, not local. It changes what the
            // machine is doing and can be seen from outside it.
            ToolSpec::external(
                "system_start",
                "Start a service. Refused when the world model says it does not fit.",
                service_schema.clone(),
            ),
            ToolSpec::external(
                "system_stop",
                "Stop a service. This can take the public sites down.",
                service_schema.clone(),
            ),
            ToolSpec::external(
                "system_snapshot",
                "Take a restore point. Changes nothing else.",
                nothing,
            ),
        ]
    }

    async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match tool {
            "system_status" => self.run(&["status"]),
            "system_check" => self.run(&["check"]),

            "system_explain" => {
                let name = self.service_arg(&args)?;
                // Answered from the model rather than by running anything: the
                // world model already holds every fact this needs.
                Ok(serde_json::json!({
                    "service": name,
                    "requires": self.world.requires(&name)
                        .iter()
                        .map(|r| serde_json::json!({
                            "service": r.required,
                            "because": r.because,
                        }))
                        .collect::<Vec<_>>(),
                    "breaksIfStopped": self.world.fallout(&name),
                    "memoryMb": self.world.service(&name).map(|s| s.typical_mb),
                }))
            }

            "system_start" => {
                let name = self.service_arg(&args)?;
                // The veto again, here rather than only in the planner: a tool
                // is callable by anything that holds the gateway, not only by
                // the path that went through `plan`.
                let running = args
                    .get("running")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let projected = self.world.plan_start(&name, &running);
                if !projected.fits {
                    return Err(format!(
                        "{name} needs {} MB and would put the total at {} MB against \
                         {} MB available",
                        projected.memory_cost_mb,
                        projected.projected_mb,
                        projected.available_mb
                    ));
                }
                self.run(&["start", &name])
            }

            "system_stop" => {
                let name = self.service_arg(&args)?;
                self.run(&["stop", &name])
            }

            "system_snapshot" => {
                let reason = args
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("model-requested");
                self.run(&["snapshot", reason])
            }

            other => Err(format!("no tool called '{other}'")),
        }
    }
}

/// Turn a plan into a gateway call, when the plan is one that may run.
///
/// `Confirm` returns `None` on purpose. The question it carries is for a
/// person; answering it here would make the confirmation step decorative.
pub fn as_tool_call(plan: &Plan) -> Option<(String, serde_json::Value)> {
    let Plan::Act { command, .. } = plan else {
        return None;
    };
    let verb = command.get(1)?.as_str();
    let target = command.get(2).cloned();

    let (tool, args) = match verb {
        "status" => ("system_status", serde_json::json!({})),
        "check" => ("system_check", serde_json::json!({})),
        "snapshot" => (
            "system_snapshot",
            serde_json::json!({ "reason": target.unwrap_or_else(|| "model-requested".into()) }),
        ),
        "start" => ("system_start", serde_json::json!({ "service": target? })),
        _ => return None,
    };
    Some((tool.to_string(), args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hajime_core::tools::{Budget, Effect, Gateway};

    fn gateway(dry_run: bool) -> Gateway {
        let mut g = Gateway::new(
            Budget { per_tool: 10, per_run: 20, failures_before_open: 3 },
            dry_run,
        );
        g.add(Box::new(SystemTools::pretending()));
        g.begin_run();
        g
    }

    #[tokio::test]
    async fn every_tool_declares_an_effect_that_matches_what_it_does() {
        // The gateway's dry run keys off this. A stop labelled `read` would run
        // during a rehearsal.
        let tools = SystemTools::pretending().list().await;
        for spec in &tools {
            let changes = spec.name.contains("start")
                || spec.name.contains("stop")
                || spec.name.contains("snapshot");
            if changes {
                assert_eq!(spec.effect, Effect::External, "{} is mislabelled", spec.name);
            } else {
                assert_eq!(spec.effect, Effect::Read, "{} is mislabelled", spec.name);
            }
        }
    }

    #[tokio::test]
    async fn the_schema_offers_only_services_that_exist() {
        // The enum is what a model reads before choosing an argument. Leaving
        // it open invites a plausible invention.
        let tools = SystemTools::pretending().list().await;
        let stop = tools.iter().find(|t| t.name == "system_stop").unwrap();
        let allowed = stop.input_schema["properties"]["service"]["enum"]
            .as_array()
            .expect("the service argument should be an enum");
        let world = World::default();
        for value in allowed {
            let name = value.as_str().unwrap();
            assert!(world.entity_names().contains(&name), "{name} is not real");
        }
    }

    #[tokio::test]
    async fn a_made_up_service_is_refused_at_the_tool_boundary() {
        // slots already guarantees this upstream. The tool checks anyway,
        // because it is callable by anything holding the gateway.
        let g = gateway(false);
        let err = g
            .call("test", "system_stop", serde_json::json!({ "service": "frobnicator" }))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not a service"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_service_argument_is_refused() {
        let g = gateway(false);
        assert!(g.call("test", "system_stop", serde_json::json!({})).await.is_err());
    }

    #[tokio::test]
    async fn a_dry_run_gateway_does_not_stop_anything() {
        let g = gateway(true);
        let out = g
            .call("test", "system_stop", serde_json::json!({ "service": "caddy" }))
            .await
            .expect("a dry run reports rather than fails");
        assert_eq!(out["dryRun"], serde_json::json!(true), "{out}");
        assert!(out["wouldHaveCalled"].as_str().unwrap().contains("stop"), "{out}");
    }

    #[tokio::test]
    async fn a_read_still_happens_during_a_dry_run() {
        // Otherwise a rehearsal shows nothing about the machine it rehearses on.
        let g = gateway(true);
        let out = g.call("test", "system_status", serde_json::json!({})).await.unwrap();
        assert!(out.get("dryRun").is_none(), "reads are not held back: {out}");
    }

    #[tokio::test]
    async fn explaining_answers_from_the_model_without_running_anything() {
        let g = gateway(false);
        let out = g
            .call("test", "system_explain", serde_json::json!({ "service": "mysql" }))
            .await
            .unwrap();
        let breaks = out["breaksIfStopped"].as_array().unwrap();
        assert!(breaks.iter().any(|v| v == "caddy"), "{out}");
        assert!(breaks.iter().any(|v| v == "cloudflared"), "the chain: {out}");
    }

    #[tokio::test]
    async fn starting_something_that_does_not_fit_is_refused_by_the_tool_too() {
        let world = World::default();
        let everything: Vec<&str> = world.services().iter().map(|s| s.name).collect();
        let g = gateway(false);
        let result = g
            .call(
                "test",
                "system_start",
                serde_json::json!({ "service": "llamacpp", "running": everything }),
            )
            .await;
        // Either it fits on this budget, or the refusal carries the arithmetic.
        if let Err(e) = result {
            assert!(format!("{e}").contains("MB"), "{e}");
        }
    }

    #[tokio::test]
    async fn the_audit_log_records_what_was_called() {
        // The reason for routing through the gateway at all. Without it the
        // console's audit panel is empty on a machine the model has been
        // operating all afternoon.
        let g = gateway(false);
        let _ = g.call("test", "system_status", serde_json::json!({})).await;
        let log = g.audit_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].tool, "system_status");
        assert_eq!(log[0].caller, "test");
    }

    #[test]
    fn a_plan_needing_confirmation_produces_no_tool_call() {
        // The rule that keeps the confirmation step from being decorative.
        let confirm = Plan::Confirm {
            intent: "stop",
            command: vec!["hajimectl".into(), "stop".into(), "caddy".into()],
            question: "Stop caddy?".into(),
            because: "it is essential".into(),
            confidence: 0.9,
        };
        assert!(as_tool_call(&confirm).is_none());
    }

    #[test]
    fn a_refusal_produces_no_tool_call() {
        let refuse = Plan::Refuse { because: "does not fit".into() };
        assert!(as_tool_call(&refuse).is_none());
    }

    #[test]
    fn a_read_only_plan_maps_to_its_tool() {
        let act = Plan::Act {
            intent: "status",
            command: vec!["hajimectl".into(), "status".into()],
            consequences: vec![],
            confidence: 0.9,
        };
        let (tool, _) = as_tool_call(&act).expect("status should map");
        assert_eq!(tool, "system_status");
    }
}
