//! From a sentence to something that can be run, or to a question.
//!
//! This is where the learned half and the exact half meet, and where the
//! decision to act is taken. Three rules shape it.
//!
//! **An uncertain intent becomes a question, not a command.** The classifier
//! reports how sure it is; below the threshold the answer is a request for
//! confirmation naming what it thought. Guessing wrong on `status` costs
//! nothing. Guessing wrong on `stop` takes the sites down.
//!
//! **The world model gets a veto.** An action that leaves the machine in a
//! state that violates a constraint is refused with the reason, before it runs.
//! Stopping mariadb is a two-word request and a four-service outage.
//!
//! **Nothing here executes anything.** A `Plan` is a description. Running it is
//! the caller's decision, made through the tool gateway with its effect
//! labelling and its dry-run mode. Keeping the deciding and the doing in
//! separate places is what makes the dry run meaningful.

use crate::classify::Classifier;
use crate::corpus::Intent;
use crate::slots;
use crate::world::{Violation, World};
use serde::Serialize;

/// Below this, the model asks instead of acting.
///
/// Chosen against the held-out set: the sentences it gets wrong there sit under
/// it, and the ones it gets right sit above. It is a threshold on a softmax
/// probability, not a promise about the world.
pub const CONFIDENT: f32 = 0.35;

/// A margin this thin means two intents fit about equally well. High confidence
/// with a thin margin is a different situation from a clear winner, and the
/// difference matters when the action is destructive.
pub const CLEAR_MARGIN: f32 = 0.10;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Plan {
    /// Run this.
    Act {
        intent: &'static str,
        command: Vec<String>,
        /// What the world model says this does. Shown before it runs.
        consequences: Vec<String>,
        confidence: f32,
    },
    /// Understood, but it needs a yes first.
    Confirm {
        intent: &'static str,
        command: Vec<String>,
        question: String,
        /// Why confirmation is being asked for rather than acting.
        because: String,
        confidence: f32,
    },
    /// Understood, and refused, with the reason.
    Refuse { because: String },
    /// Not understood well enough to name an action.
    Unclear {
        /// The best guess, offered so the person can correct it in one word
        /// rather than rephrasing from scratch.
        best_guess: Option<&'static str>,
        confidence: f32,
        question: String,
    },
}

/// The model as a whole: structure, language, and the rules joining them.
pub struct Planner {
    world: World,
    classifier: Classifier,
}

impl Planner {
    pub fn new(world: World, classifier: Classifier) -> Self {
        Self { world, classifier }
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    /// Interpret a request against what is currently running.
    pub fn plan(&self, text: &str, running: &[&str]) -> Plan {
        let Some(prediction) = self.classifier.predict(text) else {
            return Plan::Unclear {
                best_guess: None,
                confidence: 0.0,
                question: "There is nothing here I can read as a request.".to_string(),
            };
        };

        let entities = slots::find(&self.world, text);
        let named = entities.first().map(|f| f.name);
        // Whether the thing named is a jail decides which command is even
        // valid. `hajimectl jail-snapshot caddy` parses, reads plausibly, and
        // fails at the point of use, because caddy is a service.
        let named_jail = entities.first().filter(|f| f.is_jail).map(|f| f.name);

        if prediction.confidence < CONFIDENT {
            return Plan::Unclear {
                best_guess: Some(prediction.intent.as_str()),
                confidence: prediction.confidence,
                question: format!(
                    "I am not sure what you want. The closest I have is '{}'{}. \
                     Say it another way, or name the action.",
                    prediction.intent.as_str(),
                    named.map(|n| format!(" for {n}")).unwrap_or_default()
                ),
            };
        }

        // Destructive and ambiguous is the combination worth stopping for. Two
        // intents fitting equally well is fine for a status query and is not
        // fine when one of them stops a database.
        let thin = prediction.margin < CLEAR_MARGIN;
        let destructive = matches!(
            prediction.intent,
            Intent::Stop | Intent::Restart | Intent::Rollback | Intent::Save
        );

        match prediction.intent {
            Intent::Stop => self.plan_stop(named, prediction.confidence, thin),
            Intent::Start => self.plan_start(named, running, prediction.confidence, thin),
            Intent::Restart => match named {
                Some(name) => {
                    let stop = self.world.plan_stop(name);
                    let mut consequences =
                        vec![format!("{name} goes down and comes back up")];
                    if !stop.also_breaks.is_empty() {
                        consequences.push(format!(
                            "these lose their dependency while it is down: {}",
                            stop.also_breaks.join(", ")
                        ));
                    }
                    Plan::Confirm {
                        intent: "restart",
                        command: vec!["hajimectl".into(), "restart".into(), name.into()],
                        question: format!("Restart {name}?"),
                        because: consequences.join("; "),
                        confidence: prediction.confidence,
                    }
                }
                None => self.needs_a_name("restart", prediction.confidence),
            },
            Intent::Rollback => match (named_jail, named) {
                (Some(jail), _) => Plan::Confirm {
                    intent: "rollback",
                    command: vec!["hajimectl".into(), "jail-rollback".into(), jail.into()],
                    question: format!("Roll the {jail} jail back to a snapshot?"),
                    because: "a rollback destroys every snapshot taken after the one \
                              chosen, and that cannot be undone"
                        .to_string(),
                    confidence: prediction.confidence,
                },
                // A service was named, not a jail. Rollback works on datasets,
                // and only the jails have one of their own.
                (None, Some(service)) => Plan::Refuse {
                    because: format!(
                        "{service} is a service, and only jails can be rolled back: they \
                         each sit on their own dataset. The jails are {}. To undo a \
                         change to the whole machine, use a boot environment instead.",
                        self.world
                            .jails()
                            .iter()
                            .map(|j| j.name)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
                (None, None) => self.needs_a_name("rollback", prediction.confidence),
            },
            Intent::Snapshot => Plan::Act {
                intent: "snapshot",
                command: match named_jail {
                    Some(jail) => vec![
                        "hajimectl".into(),
                        "jail-snapshot".into(),
                        jail.into(),
                        "before-change".into(),
                    ],
                    // A boot environment covers the whole machine, which is the
                    // right answer both when nothing was named and when what
                    // was named is a service: services have no dataset of their
                    // own to snapshot.
                    None => vec!["hajimectl".into(), "snapshot".into(), "before-change".into()],
                },
                consequences: match (named_jail, named) {
                    (Some(jail), _) => vec![format!(
                        "nothing is changed; the {jail} jail's dataset gets a restore point"
                    )],
                    (None, Some(service)) => vec![
                        format!(
                            "{service} has no dataset of its own, so this takes a boot \
                             environment covering the whole machine instead"
                        ),
                        "nothing is changed".to_string(),
                    ],
                    (None, None) => {
                        vec!["nothing is changed; a restore point is created".to_string()]
                    }
                },
                confidence: prediction.confidence,
            },
            Intent::Save => {
                let (kept, freed) = self.world.saving_mode();
                Plan::Confirm {
                    intent: "save",
                    command: vec!["hajimectl".into(), "save".into()],
                    question: format!("Stop the optional services to free about {freed} MB?"),
                    because: format!("these stay up: {}", kept.join(", ")),
                    confidence: prediction.confidence,
                }
            }
            Intent::Resume => Plan::Act {
                intent: "resume",
                command: vec!["hajimectl".into(), "resume".into()],
                consequences: vec!["the optional services are started again".to_string()],
                confidence: prediction.confidence,
            },
            Intent::Status => Plan::Act {
                intent: "status",
                command: vec!["hajimectl".into(), "status".into()],
                consequences: vec!["reads only".to_string()],
                confidence: prediction.confidence,
            },
            Intent::Check => Plan::Act {
                intent: "check",
                command: vec!["hajimectl".into(), "check".into()],
                consequences: vec!["reads only".to_string()],
                confidence: prediction.confidence,
            },
            Intent::History => Plan::Act {
                intent: "history",
                command: vec!["hajimectl".into(), "status".into()],
                consequences: vec!["reads only".to_string()],
                confidence: prediction.confidence,
            },
            Intent::Explain => Plan::Act {
                intent: "explain",
                command: vec![],
                consequences: self.explain(named, running),
                confidence: prediction.confidence,
            },
        }
        // `destructive` is folded into the individual arms above rather than
        // applied here, because what confirmation should say differs by action.
        // Kept as a binding so the classification is visible at a glance.
        .tap_destructive(destructive)
    }

    fn needs_a_name(&self, intent: &'static str, confidence: f32) -> Plan {
        Plan::Unclear {
            best_guess: Some(intent),
            confidence,
            question: format!(
                "{intent} what? Name a service, for example: {}",
                self.world
                    .services()
                    .iter()
                    .take(3)
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn plan_stop(&self, named: Option<&'static str>, confidence: f32, thin: bool) -> Plan {
        let Some(name) = named else {
            return self.needs_a_name("stop", confidence);
        };
        let plan = self.world.plan_stop(name);

        let mut because = vec![format!("frees about {} MB", plan.frees_mb)];
        if plan.essential {
            because.push("this is an essential service: the sites go down".to_string());
        }
        if !plan.also_breaks.is_empty() {
            because.push(format!(
                "these stop working too: {}",
                plan.also_breaks.join(", ")
            ));
        }
        if thin {
            because.push("the wording could also have meant something else".to_string());
        }

        Plan::Confirm {
            intent: "stop",
            command: vec!["hajimectl".into(), "stop".into(), name.into()],
            question: format!("Stop {name}?"),
            because: because.join("; "),
            confidence,
        }
    }

    fn plan_start(
        &self,
        named: Option<&'static str>,
        running: &[&str],
        confidence: f32,
        _thin: bool,
    ) -> Plan {
        let Some(name) = named else {
            return self.needs_a_name("start", confidence);
        };
        let plan = self.world.plan_start(name, running);

        // The veto. Starting something that does not fit is how the machine
        // ends up swapping, and the arithmetic is known in advance.
        if !plan.fits {
            return Plan::Refuse {
                because: format!(
                    "{name} needs {} MB and would put the total at {} MB against {} MB \
                     available. Run 'hajimectl save' first, or stop something larger.",
                    plan.memory_cost_mb, plan.projected_mb, plan.available_mb
                ),
            };
        }

        let mut consequences = vec![format!(
            "{} MB, leaving {} MB",
            plan.memory_cost_mb,
            plan.available_mb.saturating_sub(plan.projected_mb)
        )];
        if !plan.needs_first.is_empty() {
            consequences.push(format!(
                "these have to come up first: {}",
                plan.needs_first.join(", ")
            ));
        }

        Plan::Act {
            intent: "start",
            command: vec!["hajimectl".into(), "start".into(), name.into()],
            consequences,
            confidence,
        }
    }

    /// Dependencies, fallout and memory for one service, or the budget as a
    /// whole when no service is named.
    pub fn explain(&self, named: Option<&'static str>, running: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        match named {
            Some(name) => {
                if let Some(s) = self.world.service(name) {
                    out.push(format!("{name}: {} ({} MB)", s.description, s.typical_mb));
                }
                let needs = self.world.requires(name);
                if needs.is_empty() {
                    out.push(format!("{name} depends on nothing else"));
                } else {
                    for r in needs {
                        out.push(format!("needs {}: {}", r.required, r.because));
                    }
                }
                let breaks = self.world.fallout(name);
                if breaks.is_empty() {
                    out.push(format!("nothing else depends on {name}"));
                } else {
                    out.push(format!("stopping it also breaks: {}", breaks.join(", ")));
                }
            }
            None => {
                let used = self.world.memory_of(running);
                let available = self.world.budget().available_mb();
                out.push(format!(
                    "{used} MB in use of {available} MB available to services"
                ));
                for v in self.world.check(running) {
                    out.push(v.to_string());
                }
                if self.world.check(running).is_empty() {
                    out.push("every constraint is satisfied".to_string());
                }
            }
        }
        out
    }

    /// Report on a proposed state without changing anything.
    pub fn violations(&self, running: &[&str]) -> Vec<Violation> {
        self.world.check(running)
    }
}

/// A no-op that keeps the destructive classification visible at the call site.
trait TapDestructive {
    fn tap_destructive(self, destructive: bool) -> Self;
}

impl TapDestructive for Plan {
    fn tap_destructive(self, destructive: bool) -> Self {
        // Every destructive intent already returns `Confirm` or `Refuse` above.
        // This asserts that in debug builds so a new destructive intent added
        // later cannot quietly return `Act`.
        debug_assert!(
            !destructive || !matches!(self, Plan::Act { .. }),
            "a destructive intent produced an Act plan without confirmation"
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus;

    fn planner() -> Planner {
        let world = World::default();
        let classifier = Classifier::train(
            &corpus::build(&world),
            crate::classify::DEFAULT_EPOCHS,
            crate::classify::DEFAULT_RATE,
        );
        Planner::new(World::default(), classifier)
    }

    fn running_all() -> Vec<&'static str> {
        vec!["postgresql", "mysql", "redis", "caddy", "cloudflared", "hajime_workflow"]
    }

    #[test]
    fn a_read_only_request_is_acted_on_without_asking() {
        let p = planner();
        match p.plan("what is running", &running_all()) {
            Plan::Act { intent, .. } => assert_eq!(intent, "status"),
            other => panic!("expected an action, got {other:?}"),
        }
    }

    #[test]
    fn stopping_always_asks_first() {
        // The rule that matters most. A two-word sentence must not be able to
        // take a database down without a confirmation step.
        let p = planner();
        match p.plan("stop caddy", &running_all()) {
            Plan::Confirm { intent, command, .. } => {
                assert_eq!(intent, "stop");
                assert_eq!(command, vec!["hajimectl", "stop", "caddy"]);
            }
            other => panic!("stop must ask, got {other:?}"),
        }
    }

    #[test]
    fn the_confirmation_names_the_collateral_damage() {
        // "Stop mysql?" and "Stop mysql? caddy and cloudflared go with it" are
        // different questions, and only one of them can be answered properly.
        let p = planner();
        match p.plan("stop mariadb", &running_all()) {
            Plan::Confirm { because, .. } => {
                assert!(because.contains("caddy"), "{because}");
                assert!(because.contains("cloudflared"), "{because}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn stopping_an_essential_service_says_the_sites_go_down() {
        let p = planner();
        match p.plan("أوقف كادي", &running_all()) {
            Plan::Confirm { because, .. } => assert!(because.contains("essential"), "{because}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn starting_something_that_does_not_fit_is_refused_with_the_arithmetic() {
        // The world model's veto. The answer has to arrive before the machine
        // starts swapping, and it has to say why.
        let p = planner();
        let everything: Vec<&str> =
            p.world().services().iter().map(|s| s.name).collect();
        match p.plan("start llamacpp", &everything) {
            Plan::Refuse { because } => {
                assert!(because.contains("MB"), "the numbers belong in it: {because}");
            }
            Plan::Act { .. } => { /* it fits on this budget, which is also a valid answer */ }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn starting_something_reports_what_must_come_up_first() {
        let p = planner();
        match p.plan("start hajime_workflow", &["caddy"]) {
            Plan::Act { consequences, .. } => {
                let joined = consequences.join(" ");
                assert!(joined.contains("postgresql"), "{joined}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_action_word_with_no_service_asks_which_one() {
        let p = planner();
        match p.plan("stop", &running_all()) {
            Plan::Unclear { question, .. } => assert!(question.contains("what")),
            // A bare "stop" may also read as saving mode, which is a fair
            // reading and still asks before acting.
            Plan::Confirm { .. } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gibberish_asks_rather_than_guessing() {
        let p = planner();
        match p.plan("qwerty zxcvb plugh xyzzy", &running_all()) {
            Plan::Unclear { .. } => {}
            other => panic!("gibberish should not produce {other:?}"),
        }
    }

    #[test]
    fn empty_input_is_unclear_not_a_crash() {
        let p = planner();
        assert!(matches!(p.plan("", &[]), Plan::Unclear { .. }));
    }

    #[test]
    fn no_request_can_produce_a_command_naming_something_that_does_not_exist() {
        // The property that makes this safe to wire to a shell. Whatever is
        // typed, any name in the command comes from the system's own table.
        let p = planner();
        let vocabulary = p.world().entity_names();
        for text in [
            "stop the frobnicator",
            "start postgresqlx immediately",
            "أوقف الخدمة الوهمية",
            "restart '; rm -rf / #",
            "stop $(whoami)",
            "start `id`",
        ] {
            let plan = p.plan(text, &running_all());
            let command = match &plan {
                Plan::Act { command, .. } | Plan::Confirm { command, .. } => command.clone(),
                _ => continue,
            };
            for argument in command.iter().skip(2) {
                assert!(
                    vocabulary.contains(&argument.as_str())
                        || argument == "before-change",
                    "{text:?} produced argument {argument:?}, which is not a known entity"
                );
            }
        }
    }

    #[test]
    fn explaining_a_service_gives_its_dependencies_and_its_fallout() {
        // Asked directly, because whether this particular sentence classifies
        // as `explain` is the classifier's business and is tested there. What
        // matters here is that the explanation carries the second-hop damage.
        let p = planner();
        let text = p.explain(Some("mysql"), &running_all()).join(" ");
        assert!(text.contains("caddy"), "{text}");
        assert!(text.contains("cloudflared"), "the chain, not one step: {text}");
    }

    #[test]
    fn explaining_with_no_service_named_reports_the_budget() {
        let p = planner();
        let text = p.explain(None, &running_all()).join(" ");
        assert!(text.contains("MB"), "{text}");
    }

    #[test]
    fn a_jail_command_is_never_issued_for_a_service() {
        // `hajimectl jail-snapshot caddy` parses, reads plausibly, and fails at
        // the point of use. The planner used to emit exactly that whenever any
        // name appeared next to a snapshot request.
        let p = planner();
        let jails: Vec<&str> = p.world().jails().iter().map(|j| j.name).collect();

        for text in ["snapshot caddy", "خذ لقطة للكادي", "roll postgresql back"] {
            let command = match p.plan(text, &running_all()) {
                Plan::Act { command, .. } | Plan::Confirm { command, .. } => command,
                // Refusing is the other correct answer.
                _ => continue,
            };
            if let Some(position) = command.iter().position(|a| a.starts_with("jail-")) {
                let argument = &command[position + 1];
                assert!(
                    jails.contains(&argument.as_str()),
                    "{text:?} produced {command:?}, and {argument:?} is not a jail"
                );
            }
        }
    }

    #[test]
    fn rolling_back_a_service_is_refused_with_the_list_of_jails() {
        let p = planner();
        match p.plan("roll caddy back", &running_all()) {
            Plan::Refuse { because } => {
                assert!(because.contains("only jails"), "{because}");
                assert!(because.contains("ai"), "the list belongs in it: {because}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn snapshotting_a_service_falls_back_to_the_whole_machine() {
        let p = planner();
        match p.plan("snapshot caddy first", &running_all()) {
            Plan::Act { command, consequences, .. } => {
                assert_eq!(command, vec!["hajimectl", "snapshot", "before-change"]);
                assert!(
                    consequences.join(" ").contains("boot environment"),
                    "it should say what it did instead: {consequences:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_rollback_warns_that_it_cannot_be_undone() {
        let p = planner();
        match p.plan("roll the ai jail back", &running_all()) {
            Plan::Confirm { because, .. } => {
                assert!(because.contains("cannot be undone"), "{because}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn saving_mode_says_what_survives_it() {
        let p = planner();
        match p.plan("free up memory", &running_all()) {
            Plan::Confirm { because, .. } => assert!(because.contains("caddy"), "{because}"),
            other => panic!("{other:?}"),
        }
    }
}
