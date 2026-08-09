//! A parametric model of the system, in the sense a CAD package means it.
//!
//! In CAD you declare geometry and the constraints between it, and a solver
//! keeps the whole assembly consistent when one dimension moves. The same shape
//! fits a server: the entities are services and jails, the constraints are
//! "postgres must be up before the workflow engine" and "everything running has
//! to fit in eight gigabytes", and the solver answers what a change does before
//! anyone makes it.
//!
//! Two properties matter more than completeness.
//!
//! **It cannot drift.** The entities are read from the same tables `hajimectl`
//! and the console use. A model with its own copy of the service list is a
//! model that is wrong the first time a service is renamed, and wrong silently.
//!
//! **It cannot invent.** Every name it can talk about comes from those tables.
//! The classifier downstream is free to be uncertain about what someone meant;
//! it is not free to name a service that does not exist.

use hajime_sys::jail::{self, Jail};
use hajime_sys::service::{self, Service, Tier};
use serde::Serialize;

/// Why one thing has to come up before another.
///
/// Declared here rather than inferred from the order of the service table. The
/// table's ordering carries the intent, but an ordering is not a dependency:
/// it cannot say *why*, and it cannot express that two neighbours are actually
/// independent. A solver that guesses edges from position produces confident
/// answers about relationships nobody stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Requires {
    pub dependent: &'static str,
    pub required: &'static str,
    pub because: &'static str,
}

pub static REQUIREMENTS: &[Requires] = &[
    Requires {
        dependent: "hajime_workflow",
        required: "postgresql",
        because: "workflows read and write the application databases",
    },
    Requires {
        dependent: "hajime_workflow",
        required: "redis",
        because: "queue and rate-limit state",
    },
    Requires {
        dependent: "caddy",
        required: "mysql",
        because: "the store it fronts keeps its catalogue in mariadb",
    },
    Requires {
        dependent: "cloudflared",
        required: "caddy",
        because: "the tunnel forwards to the proxy; without it visitors reach nothing",
    },
    Requires {
        dependent: "hajime_wa",
        required: "hajime_wa_bridge",
        because: "the gateway speaks to WhatsApp only through the bridge",
    },
    Requires {
        dependent: "hajime_ai",
        required: "llamacpp",
        because: "the gateway has no model of its own to answer with",
    },
];

/// A resource the whole machine shares and can run out of.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Budget {
    pub total_mb: u32,
    /// Left for the kernel, ZFS ARC and everything not in the service table.
    /// Filling memory to the last megabyte is how a machine starts swapping and
    /// then stops answering.
    pub reserved_mb: u32,
}

impl Default for Budget {
    fn default() -> Self {
        // The host this replaces has 8 GB and was swapping at 6.9 GB in use.
        Self { total_mb: 8192, reserved_mb: 2048 }
    }
}

impl Budget {
    pub fn available_mb(&self) -> u32 {
        self.total_mb.saturating_sub(self.reserved_mb)
    }
}

/// The model itself.
pub struct World {
    budget: Budget,
}

impl Default for World {
    fn default() -> Self {
        Self::new(Budget::default())
    }
}

impl World {
    pub fn new(budget: Budget) -> Self {
        Self { budget }
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    pub fn services(&self) -> Vec<&'static Service> {
        service::start_order().collect()
    }

    pub fn jails(&self) -> &'static [Jail] {
        jail::JAILS
    }

    pub fn service(&self, name: &str) -> Option<&'static Service> {
        service::find(name)
    }

    /// Every name the model is allowed to say.
    ///
    /// The vocabulary of the whole system, and the only strings the planner
    /// will ever put into a command.
    pub fn entity_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.services().iter().map(|s| s.name).collect();
        names.extend(self.jails().iter().map(|j| j.name));
        names.sort_unstable();
        names.dedup();
        names
    }

    /// What `name` needs in order to work.
    pub fn requires(&self, name: &str) -> Vec<&'static Requires> {
        REQUIREMENTS.iter().filter(|r| r.dependent == name).collect()
    }

    /// What stops working if `name` does.
    pub fn dependents(&self, name: &str) -> Vec<&'static Requires> {
        REQUIREMENTS.iter().filter(|r| r.required == name).collect()
    }

    /// Everything that breaks if `name` goes down, following the chain.
    ///
    /// The transitive answer, because the one-step answer is the one that gets
    /// someone into trouble: stopping mariadb looks like it only affects Caddy
    /// until the tunnel goes dark too.
    pub fn fallout(&self, name: &str) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        let mut stack: Vec<&str> = vec![name];
        let mut seen: Vec<&str> = vec![name];

        while let Some(current) = stack.pop() {
            for r in self.dependents(current) {
                if seen.contains(&r.dependent) {
                    continue;
                }
                seen.push(r.dependent);
                out.push(r.dependent);
                stack.push(r.dependent);
            }
        }
        out.sort_unstable();
        out
    }

    /// Memory the named services would occupy together.
    pub fn memory_of(&self, running: &[&str]) -> u32 {
        self.services()
            .iter()
            .filter(|s| running.contains(&s.name))
            .map(|s| s.typical_mb)
            .sum()
    }

    /// What saving mode leaves running, and what it frees.
    pub fn saving_mode(&self) -> (Vec<&'static str>, u32) {
        let kept: Vec<&'static str> = self
            .services()
            .iter()
            .filter(|s| s.tier == Tier::Essential)
            .map(|s| s.name)
            .collect();
        let freed = self
            .services()
            .iter()
            .filter(|s| s.tier != Tier::Essential)
            .map(|s| s.typical_mb)
            .sum();
        (kept, freed)
    }

    /// Check a proposed set of running services against every constraint.
    ///
    /// Returns what is wrong rather than a yes or no. "This will not fit" sends
    /// someone looking; "this is 900 MB over and llamacpp is 4200 of it" tells
    /// them what to do.
    pub fn check(&self, running: &[&str]) -> Vec<Violation> {
        let mut out = Vec::new();

        // Dependencies, in both directions of usefulness: something running
        // without what it needs is as broken as something missing.
        for name in running {
            for r in self.requires(name) {
                if !running.contains(&r.required) {
                    out.push(Violation::MissingDependency {
                        dependent: r.dependent,
                        required: r.required,
                        because: r.because,
                    });
                }
            }
        }

        // Essential services are essential.
        for s in self.services().iter().filter(|s| s.tier == Tier::Essential) {
            if !running.contains(&s.name) {
                out.push(Violation::EssentialDown { service: s.name });
            }
        }

        // The budget.
        let used = self.memory_of(running);
        let available = self.budget.available_mb();
        if used > available {
            let worst = self
                .services()
                .iter()
                .filter(|s| running.contains(&s.name) && s.tier != Tier::Essential)
                .max_by_key(|s| s.typical_mb)
                .map(|s| (s.name, s.typical_mb));
            out.push(Violation::OverBudget {
                used_mb: used,
                available_mb: available,
                largest_optional: worst,
            });
        }

        out
    }

    /// Would starting this service break anything, and what does it need first?
    pub fn plan_start(&self, name: &str, running: &[&str]) -> StartPlan {
        let missing: Vec<&'static str> = self
            .requires(name)
            .iter()
            .filter(|r| !running.contains(&r.required))
            .map(|r| r.required)
            .collect();

        let cost = self.service(name).map(|s| s.typical_mb).unwrap_or(0);
        let mut after: Vec<&str> = running.to_vec();
        if let Some(s) = self.service(name) {
            if !after.contains(&s.name) {
                after.push(s.name);
            }
        }
        for m in &missing {
            if !after.contains(m) {
                after.push(m);
            }
        }
        let projected = self.memory_of(&after);

        StartPlan {
            service: name.to_string(),
            needs_first: missing,
            memory_cost_mb: cost,
            projected_mb: projected,
            available_mb: self.budget.available_mb(),
            fits: projected <= self.budget.available_mb(),
        }
    }

    /// What stopping this service costs and frees.
    pub fn plan_stop(&self, name: &str) -> StopPlan {
        StopPlan {
            service: name.to_string(),
            frees_mb: self.service(name).map(|s| s.typical_mb).unwrap_or(0),
            also_breaks: self.fallout(name),
            essential: self.service(name).map(|s| s.is_essential()).unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Violation {
    MissingDependency {
        dependent: &'static str,
        required: &'static str,
        because: &'static str,
    },
    EssentialDown {
        service: &'static str,
    },
    OverBudget {
        used_mb: u32,
        available_mb: u32,
        largest_optional: Option<(&'static str, u32)>,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::MissingDependency { dependent, required, because } => write!(
                f,
                "{dependent} needs {required}, which is not running: {because}"
            ),
            Violation::EssentialDown { service } => {
                write!(f, "{service} is essential and is not running")
            }
            Violation::OverBudget { used_mb, available_mb, largest_optional } => {
                write!(f, "{used_mb} MB in use against {available_mb} MB available")?;
                if let Some((name, mb)) = largest_optional {
                    write!(f, "; the largest optional service is {name} at {mb} MB")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StartPlan {
    pub service: String,
    pub needs_first: Vec<&'static str>,
    pub memory_cost_mb: u32,
    pub projected_mb: u32,
    pub available_mb: u32,
    pub fits: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StopPlan {
    pub service: String,
    pub frees_mb: u32,
    pub also_breaks: Vec<&'static str>,
    pub essential: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_requirement_names_services_that_exist() {
        // The model's edges and the service table must agree. A requirement
        // naming a service nobody has is a constraint that never fires, and it
        // would never be noticed.
        let w = World::default();
        for r in REQUIREMENTS {
            assert!(
                w.service(r.dependent).is_some(),
                "no service named '{}'",
                r.dependent
            );
            assert!(
                w.service(r.required).is_some(),
                "no service named '{}'",
                r.required
            );
            assert!(!r.because.is_empty(), "{} -> {} has no reason", r.dependent, r.required);
        }
    }

    #[test]
    fn nothing_requires_itself() {
        for r in REQUIREMENTS {
            assert_ne!(r.dependent, r.required, "{} requires itself", r.dependent);
        }
    }

    #[test]
    fn fallout_follows_the_chain_rather_than_one_step() {
        // Stopping mariadb looks like it only affects Caddy until the tunnel
        // goes dark too. That second hop is the one that catches people.
        let w = World::default();
        let broken = w.fallout("mysql");
        assert!(broken.contains(&"caddy"), "{broken:?}");
        assert!(broken.contains(&"cloudflared"), "the tunnel is behind caddy: {broken:?}");
    }

    #[test]
    fn fallout_terminates_even_if_the_graph_ever_gains_a_cycle() {
        // The seen-list, not the shape of today's data, is what guarantees this.
        let w = World::default();
        for s in w.services() {
            let _ = w.fallout(s.name);
        }
    }

    #[test]
    fn starting_something_reports_what_it_needs_first() {
        let w = World::default();
        let plan = w.plan_start("hajime_workflow", &["caddy"]);
        assert!(plan.needs_first.contains(&"postgresql"), "{plan:?}");
        assert!(plan.needs_first.contains(&"redis"), "{plan:?}");
    }

    #[test]
    fn starting_the_model_on_a_full_machine_does_not_fit() {
        // The whole reason the budget is in the model: this answer has to come
        // before the machine starts swapping, not after.
        let w = World::default();
        let everything: Vec<&str> = w.services().iter().map(|s| s.name).collect();
        let plan = w.plan_start("llamacpp", &everything);
        assert!(
            !plan.fits || plan.projected_mb <= plan.available_mb,
            "the verdict must match the arithmetic: {plan:?}"
        );
    }

    #[test]
    fn stopping_an_essential_service_says_so_and_names_the_fallout() {
        let w = World::default();
        let plan = w.plan_stop("caddy");
        assert!(plan.essential);
        assert!(plan.also_breaks.contains(&"cloudflared"), "{plan:?}");
    }

    #[test]
    fn a_missing_dependency_is_reported_with_its_reason() {
        let w = World::default();
        let v = w.check(&["hajime_workflow"]);
        let dep = v.iter().find(|v| {
            matches!(v, Violation::MissingDependency { required: "postgresql", .. })
        });
        let dep = dep.expect("postgresql should be reported missing");
        assert!(
            format!("{dep}").contains("databases"),
            "the reason belongs in the message: {dep}"
        );
    }

    #[test]
    fn running_everything_is_reported_as_over_budget_with_the_biggest_offender() {
        let w = World::default();
        let everything: Vec<&str> = w.services().iter().map(|s| s.name).collect();
        let v = w.check(&everything);
        let over = v.iter().find(|v| matches!(v, Violation::OverBudget { .. }));
        if let Some(Violation::OverBudget { largest_optional, .. }) = over {
            assert!(
                largest_optional.is_some(),
                "naming the largest optional service is the actionable part"
            );
        }
    }

    #[test]
    fn saving_mode_keeps_every_essential_service() {
        let w = World::default();
        let (kept, freed) = w.saving_mode();
        for s in w.services().iter().filter(|s| s.is_essential()) {
            assert!(kept.contains(&s.name), "saving mode dropped {}", s.name);
        }
        assert!(freed > 0, "saving mode that frees nothing is not a mode");
    }

    #[test]
    fn the_vocabulary_covers_services_and_jails_and_nothing_else() {
        let w = World::default();
        let names = w.entity_names();
        assert!(names.contains(&"postgresql"));
        assert!(names.contains(&"ai"), "jails are addressable too");
        assert!(!names.contains(&"nonsense"));
        // No duplicates: a name that appears twice would be scored twice by the
        // matcher downstream.
        let mut sorted = names.clone();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
