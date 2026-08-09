//! From a broken machine to a working one.
//!
//! [`plan`](crate::plan) answers a request. This answers a different question
//! that nobody asked out loud: given what is wrong right now, what sequence of
//! actions fixes it, and in what order.
//!
//! The order is the whole difficulty. Starting the workflow engine before
//! postgres leaves it running against a database that is not there, which reads
//! as working and is not. Stopping mariadb to free memory takes Caddy and the
//! tunnel with it. A list of faults is easy; a sequence that resolves them
//! without creating new ones is the part worth writing down.
//!
//! **A plan is simulated before it is offered.** Each step is applied to a copy
//! of the state, and the result is checked against every constraint. A plan that
//! does not end in a satisfied machine is not returned as a plan; it is returned
//! as an explanation of why the machine cannot be fixed by starting and stopping
//! things. That distinction matters at three in the morning: "here are four
//! steps, and afterwards nothing is violated" is worth acting on, and "you are
//! 900 MB short and no arrangement fits" sends you to a different solution
//! instead of round a loop.

use crate::world::{Violation, World};
use serde::Serialize;

/// Something the machine got wrong, in the order it should be dealt with.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fault {
    pub what: String,
    /// Lower runs first. Dependencies before dependents, space before starts.
    pub order: u8,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The sites are down or about to be.
    Critical,
    /// Something is degraded but the public still gets served.
    Degraded,
}

/// One thing to do.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    pub command: Vec<String>,
    /// What this step is for, in a sentence a tired person can read.
    pub because: String,
    /// Does it need a yes first? Stopping does; starting does not.
    pub destructive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Repair {
    /// Nothing is wrong.
    Healthy,
    /// These steps, in this order, leave the machine satisfying every
    /// constraint. Proved by simulation before being returned.
    Plan {
        faults: Vec<Fault>,
        steps: Vec<Step>,
        /// What the machine looks like afterwards.
        ends_running: Vec<String>,
        freed_mb: u32,
    },
    /// The faults are real and starting and stopping services will not fix
    /// them. Says what is left over rather than proposing something that does
    /// not work.
    Impossible {
        faults: Vec<Fault>,
        because: String,
        /// What the best attempt achieved, so the reader can see how close it
        /// came.
        remaining: Vec<String>,
    },
}

/// Work out what is wrong and what would fix it.
pub struct Doctor<'a> {
    world: &'a World,
}

impl<'a> Doctor<'a> {
    pub fn new(world: &'a World) -> Self {
        Self { world }
    }

    /// What is wrong, worst and earliest first.
    pub fn diagnose(&self, running: &[&str]) -> Vec<Fault> {
        let mut faults: Vec<Fault> = self
            .world
            .check(running)
            .into_iter()
            .map(|v| match &v {
                // Memory first: starting anything else is pointless until
                // there is room, and the fix for it is a stop, which is the
                // step most likely to need a human.
                Violation::OverBudget { .. } => Fault {
                    what: v.to_string(),
                    order: 0,
                    severity: Severity::Critical,
                },
                // Then the things others depend on.
                Violation::MissingDependency { .. } => Fault {
                    what: v.to_string(),
                    order: 1,
                    severity: Severity::Critical,
                },
                Violation::EssentialDown { service } => Fault {
                    what: v.to_string(),
                    order: 2,
                    severity: if self.world.dependents(service).is_empty() {
                        Severity::Degraded
                    } else {
                        Severity::Critical
                    },
                },
            })
            .collect();
        faults.sort_by_key(|f| (f.order, f.what.clone()));
        faults
    }

    /// A sequence that fixes what is wrong, or an explanation that it cannot.
    pub fn repair(&self, running: &[&str]) -> Repair {
        let faults = self.diagnose(running);
        if faults.is_empty() {
            return Repair::Healthy;
        }

        let mut state: Vec<&'static str> = self
            .world
            .services()
            .iter()
            .filter(|s| running.contains(&s.name))
            .map(|s| s.name)
            .collect();
        let mut steps: Vec<Step> = Vec::new();
        let mut freed = 0u32;

        // 1. Make room. Optional services go first, largest first, because one
        //    large stop beats four small ones and each stop is a step someone
        //    has to approve.
        let available = self.world.budget().available_mb();
        if self.world.memory_of(&state) > available {
            let mut optional: Vec<_> = self
                .world
                .services()
                .iter()
                .filter(|s| !s.is_essential() && state.contains(&s.name))
                .copied()
                .collect();
            optional.sort_by_key(|s| std::cmp::Reverse(s.typical_mb));

            for s in optional {
                if self.world.memory_of(&state) <= available {
                    break;
                }
                state.retain(|n| *n != s.name);
                freed += s.typical_mb;
                steps.push(Step {
                    command: vec!["hajimectl".into(), "stop".into(), s.name.into()],
                    because: format!(
                        "frees {} MB; optional, so the sites keep serving without it",
                        s.typical_mb
                    ),
                    destructive: true,
                });
            }
        }

        // 2. Start what is missing, dependencies first. Repeated passes rather
        //    than a topological sort: the graph is a handful of edges, and a
        //    pass that changes nothing means the rest cannot be started, which
        //    is exactly the condition to stop on.
        loop {
            let mut started_something = false;

            for s in self.world.services() {
                if state.contains(&s.name) || !s.is_essential() {
                    continue;
                }
                // Only when everything it needs is already up.
                let ready = self
                    .world
                    .requires(s.name)
                    .iter()
                    .all(|r| state.contains(&r.required));
                if !ready {
                    continue;
                }
                // And only if it fits.
                let mut after = state.clone();
                after.push(s.name);
                if self.world.memory_of(&after) > available {
                    continue;
                }

                let needed_by = self.world.dependents(s.name);
                state = after;
                steps.push(Step {
                    command: vec!["hajimectl".into(), "start".into(), s.name.into()],
                    because: if needed_by.is_empty() {
                        format!("{} is essential ({} MB)", s.name, s.typical_mb)
                    } else {
                        format!(
                            "{} needs to be up before {}",
                            s.name,
                            needed_by
                                .iter()
                                .map(|r| r.dependent)
                                .collect::<Vec<_>>()
                                .join(" and ")
                        )
                    },
                    destructive: false,
                });
                started_something = true;
            }

            if !started_something {
                break;
            }
        }

        // 3. The proof. A plan that does not end in a satisfied machine is not
        //    offered as one.
        let remaining = self.world.check(&state);
        if remaining.is_empty() {
            Repair::Plan {
                faults,
                steps,
                ends_running: state.iter().map(|s| s.to_string()).collect(),
                freed_mb: freed,
            }
        } else {
            Repair::Impossible {
                faults,
                because: "starting and stopping services does not resolve this. \
                          The machine needs something changed that is not a \
                          service: more memory, or a service removed from the \
                          essential tier."
                    .to_string(),
                remaining: remaining.iter().map(|v| v.to_string()).collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        World::default()
    }

    fn all_essential() -> Vec<&'static str> {
        World::default()
            .services()
            .iter()
            .filter(|s| s.is_essential())
            .map(|s| s.name)
            .collect()
    }

    #[test]
    fn a_healthy_machine_needs_no_repair() {
        let w = world();
        let d = Doctor::new(&w);
        assert!(matches!(d.repair(&all_essential()), Repair::Healthy));
    }

    #[test]
    fn a_dead_machine_is_brought_all_the_way_back() {
        // The plan has to end satisfied, not merely do something.
        let w = world();
        let d = Doctor::new(&w);
        match d.repair(&[]) {
            Repair::Plan { steps, ends_running, .. } => {
                assert!(!steps.is_empty());
                let ends: Vec<&str> = ends_running.iter().map(String::as_str).collect();
                assert!(
                    w.check(&ends).is_empty(),
                    "the plan left violations: {:?}",
                    w.check(&ends)
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dependencies_are_started_before_the_things_that_need_them() {
        // Starting the engine before postgres leaves it running against a
        // database that is not there, which reads as working.
        let w = world();
        let d = Doctor::new(&w);
        let Repair::Plan { steps, .. } = d.repair(&[]) else {
            panic!("expected a plan");
        };
        let order: Vec<&str> = steps
            .iter()
            .filter_map(|s| s.command.get(2).map(String::as_str))
            .collect();

        for r in crate::world::REQUIREMENTS {
            let (Some(dep), Some(req)) = (
                order.iter().position(|n| *n == r.dependent),
                order.iter().position(|n| *n == r.required),
            ) else {
                continue;
            };
            assert!(
                req < dep,
                "{} is started before {}, which it needs",
                r.dependent,
                r.required
            );
        }
    }

    #[test]
    fn a_stop_is_marked_as_needing_a_yes_and_a_start_is_not() {
        // The console and the CLI both key off this. A repair that stops a
        // service without asking is a repair that can take the sites down.
        let w = world();
        let d = Doctor::new(&w);
        let everything: Vec<&str> = w.services().iter().map(|s| s.name).collect();
        if let Repair::Plan { steps, .. } = d.repair(&everything) {
            for s in &steps {
                let is_stop = s.command.get(1).map(String::as_str) == Some("stop");
                assert_eq!(s.destructive, is_stop, "{:?} is mislabelled", s.command);
            }
        }
    }

    #[test]
    fn memory_is_freed_before_anything_is_started() {
        // Starting into a full machine is how it begins swapping. The stop
        // steps have to come first.
        let w = world();
        let d = Doctor::new(&w);
        // Everything optional up, nothing essential: over budget and broken.
        let optional: Vec<&str> = w
            .services()
            .iter()
            .filter(|s| !s.is_essential())
            .map(|s| s.name)
            .collect();
        if let Repair::Plan { steps, .. } = d.repair(&optional) {
            let first_start = steps.iter().position(|s| !s.destructive);
            let last_stop = steps.iter().rposition(|s| s.destructive);
            if let (Some(f), Some(l)) = (first_start, last_stop) {
                assert!(l < f, "a start is scheduled before a stop:\n{steps:#?}");
            }
        }
    }

    #[test]
    fn an_unfixable_machine_says_so_rather_than_proposing_something_that_fails() {
        // A budget too small for the essential tier cannot be repaired by
        // starting and stopping. Offering a plan that ends violated would send
        // someone round a loop.
        let tiny = World::new(crate::world::Budget { total_mb: 1024, reserved_mb: 512 });
        let d = Doctor::new(&tiny);
        match d.repair(&[]) {
            Repair::Impossible { because, remaining, .. } => {
                assert!(because.contains("not a service"), "{because}");
                assert!(!remaining.is_empty());
            }
            Repair::Plan { ends_running, .. } => {
                let ends: Vec<&str> = ends_running.iter().map(String::as_str).collect();
                assert!(
                    tiny.check(&ends).is_empty(),
                    "a plan was returned that does not actually satisfy the world"
                );
            }
            Repair::Healthy => panic!("an empty machine is not healthy"),
        }
    }

    #[test]
    fn the_worst_fault_is_reported_first() {
        let w = world();
        let d = Doctor::new(&w);
        let faults = d.diagnose(&[]);
        assert!(!faults.is_empty());
        for pair in faults.windows(2) {
            assert!(pair[0].order <= pair[1].order, "faults are out of order");
        }
    }

    #[test]
    fn every_command_names_a_service_that_exists() {
        // The same guarantee the planner gives: nothing here can name a
        // service that is not in the table.
        let w = world();
        let d = Doctor::new(&w);
        let names = w.entity_names();
        for start in [vec![], all_essential(), vec!["caddy"]] {
            if let Repair::Plan { steps, .. } = d.repair(&start) {
                for s in &steps {
                    if let Some(target) = s.command.get(2) {
                        assert!(names.contains(&target.as_str()), "{target} is not real");
                    }
                }
            }
        }
    }

    #[test]
    fn a_partial_outage_is_repaired_without_touching_what_works() {
        // Only postgres is down. The plan should start it and not restart the
        // whole machine around it.
        let w = world();
        let d = Doctor::new(&w);
        let mut running = all_essential();
        running.retain(|n| *n != "postgresql");

        match d.repair(&running) {
            Repair::Plan { steps, .. } => {
                let touched: Vec<&str> = steps
                    .iter()
                    .filter_map(|s| s.command.get(2).map(String::as_str))
                    .collect();
                assert!(touched.contains(&"postgresql"), "{touched:?}");
                assert!(
                    !touched.contains(&"caddy"),
                    "caddy was working and should have been left alone: {touched:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
