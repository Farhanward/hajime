//! `hajime-model`: ask the system about itself, in words.
//!
//! Reads a request, says what it would do, and stops there. Running the command
//! is a separate step taken by a person or by `hajimectl`, because a model that
//! both decides and executes gives nobody a place to stand between the two.
//!
//! Usage:
//!   hajime-model ask "stop caddy"        interpret one request
//!   hajime-model ask --json "..."        the same, for another program
//!   hajime-model explain <service>       dependencies, fallout, memory
//!   hajime-model check                   constraints against what is running
//!   hajime-model repair [--json]         what is wrong and the order to fix it
//!   hajime-model train <file>            train and write the weights out
//!   hajime-model accuracy                what it scores on held-out phrasings
//!
//! The trained weights are loaded from `HAJIME_MODEL_WEIGHTS` when it is set,
//! and otherwise trained on the spot. Training takes about a tenth of a second,
//! so the fallback is not a hardship; the file exists so a deployed machine is
//! running exactly the weights that were measured, rather than whatever today's
//! corpus produces.

use hajime_model::classify::{Classifier, Compact, DEFAULT_EPOCHS, DEFAULT_RATE};
use hajime_model::plan::{Plan, Planner};
use hajime_model::{corpus, slots, world::World};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage:
  hajime-model ask [--json] <request>
  hajime-model explain <service>
  hajime-model check
  hajime-model repair [--json]
  hajime-model train <output-file>
  hajime-model accuracy"
    );
    ExitCode::from(2)
}

/// Load the shipped weights, or train.
///
/// A weights file from an older feature space is refused rather than used: it
/// would map every n-gram to the wrong weight and the only symptom would be
/// bad answers.
fn load_or_train() -> Classifier {
    // The environment first, then the conventional install path. Without the
    // second, the weights the installer trains are written and never read: a
    // command run from a shell has no reason to have the variable set.
    let candidates: Vec<String> = std::env::var("HAJIME_MODEL_WEIGHTS")
        .into_iter()
        .chain(std::iter::once("/usr/local/etc/hajime/model.json".to_string()))
        .collect();

    for path in candidates {
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            // Compact first: that is what `train` writes now. The float form
            // is still accepted so a weights file from before the change keeps
            // working rather than sending the binary back to training on every
            // invocation.
            Ok(raw) => match Compact::from_json(&raw)
                .map(|c| c.expand())
                .or_else(|_| Classifier::from_json(&raw))
            {
                Ok(model) => return model,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    eprintln!("training from the corpus instead");
                }
            },
            Err(e) => eprintln!("{path}: {e}; training from the corpus instead"),
        }
    }
    Classifier::train(&corpus::build(&World::default()), DEFAULT_EPOCHS, DEFAULT_RATE)
}

/// What is running now.
///
/// Asked of the system rather than assumed, because every answer the world
/// model gives depends on it: whether something fits, what a stop would break.
fn running_now() -> Vec<&'static str> {
    hajime_sys::service::start_order()
        .filter(|s| {
            std::process::Command::new("service")
                .args([s.rc_name(), "onestatus"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .map(|s| s.name)
        .collect()
}

fn print_plan(plan: &Plan) -> ExitCode {
    match plan {
        Plan::Act { intent, command, consequences, confidence } => {
            println!("{intent}  (confidence {confidence:.2})");
            for c in consequences {
                println!("  {c}");
            }
            if !command.is_empty() {
                println!();
                println!("  {}", command.join(" "));
            }
            ExitCode::SUCCESS
        }
        Plan::Confirm { intent: _, command, question, because, confidence } => {
            println!("{question}  (confidence {confidence:.2})");
            println!("  {because}");
            println!();
            println!("  run it yourself if that is what you meant:");
            println!("  {}", command.join(" "));
            // Not an error, but not done either. A caller scripting this needs
            // to tell "I did it" from "it needs your yes" without parsing text.
            ExitCode::from(10)
        }
        Plan::Refuse { because } => {
            println!("refused: {because}");
            ExitCode::from(1)
        }
        Plan::Unclear { best_guess, confidence, question } => {
            println!("{question}");
            if let Some(guess) = best_guess {
                println!("  (closest match '{guess}' at {confidence:.2})");
            }
            ExitCode::from(3)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return usage();
    };

    match command {
        "ask" => {
            let json = args.iter().any(|a| a == "--json");
            let request: String = args
                .iter()
                .skip(1)
                .filter(|a| *a != "--json")
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if request.trim().is_empty() {
                eprintln!("ask what?");
                return ExitCode::from(2);
            }

            let planner = Planner::new(World::default(), load_or_train());
            let running = running_now();
            let plan = planner.plan(&request, &running);

            if json {
                match serde_json::to_string_pretty(&plan) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        eprintln!("could not serialise the plan: {e}");
                        return ExitCode::FAILURE;
                    }
                }
                // The exit code still carries the outcome, so a script can
                // branch without reading the JSON.
                return match plan {
                    Plan::Act { .. } => ExitCode::SUCCESS,
                    Plan::Confirm { .. } => ExitCode::from(10),
                    Plan::Refuse { .. } => ExitCode::from(1),
                    Plan::Unclear { .. } => ExitCode::from(3),
                };
            }
            print_plan(&plan)
        }

        "explain" => {
            let Some(name) = args.get(1) else {
                eprintln!("explain what?");
                return ExitCode::from(2);
            };
            let world = World::default();
            let found = slots::find(&world, name);
            let Some(entity) = found.first() else {
                eprintln!("no service or jail called '{name}'");
                eprintln!("known: {}", world.entity_names().join(", "));
                return ExitCode::from(2);
            };
            let planner = Planner::new(World::default(), load_or_train());
            for line in planner.explain(Some(entity.name), &running_now()) {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }

        "check" => {
            let world = World::default();
            let running = running_now();
            println!("running: {}", if running.is_empty() {
                "nothing".to_string()
            } else {
                running.join(", ")
            });
            let violations = world.check(&running);
            if violations.is_empty() {
                println!("every constraint is satisfied");
                return ExitCode::SUCCESS;
            }
            for v in &violations {
                println!("  {v}");
            }
            ExitCode::FAILURE
        }

        "repair" => {
            use hajime_model::repair::{Doctor, Repair};
            let world = World::default();
            let running = running_now();
            let plan = Doctor::new(&world).repair(&running);

            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
                return match plan {
                    Repair::Healthy => ExitCode::SUCCESS,
                    Repair::Plan { .. } => ExitCode::from(10),
                    Repair::Impossible { .. } => ExitCode::FAILURE,
                };
            }

            match plan {
                Repair::Healthy => {
                    println!("nothing is wrong");
                    ExitCode::SUCCESS
                }
                Repair::Plan { faults, steps, ends_running, freed_mb } => {
                    println!("{} fault(s):", faults.len());
                    for f in &faults {
                        println!("  {:?}  {}", f.severity, f.what);
                    }
                    println!();
                    println!("{} step(s), in this order:", steps.len());
                    for (i, s) in steps.iter().enumerate() {
                        println!(
                            "  {}. {}{}",
                            i + 1,
                            s.command.join(" "),
                            if s.destructive { "   [needs your yes]" } else { "" }
                        );
                        println!("     {}", s.because);
                    }
                    println!();
                    if freed_mb > 0 {
                        println!("frees {freed_mb} MB along the way");
                    }
                    println!("afterwards: {}", ends_running.join(", "));
                    println!();
                    println!("This was checked against every constraint before being");
                    println!("printed. Nothing here has been run.");
                    // Not an error, and not done either.
                    ExitCode::from(10)
                }
                Repair::Impossible { faults, because, remaining } => {
                    println!("{} fault(s), and no sequence of starts and stops fixes them:", faults.len());
                    for f in &faults {
                        println!("  {}", f.what);
                    }
                    println!();
                    println!("{because}");
                    println!();
                    println!("what would still be wrong:");
                    for r in &remaining {
                        println!("  {r}");
                    }
                    ExitCode::FAILURE
                }
            }
        }

        "train" => {
            let Some(out) = args.get(1) else {
                eprintln!("train where? give an output path");
                return ExitCode::from(2);
            };
            let world = World::default();
            let training = corpus::build(&world);
            let started = std::time::Instant::now();
            let model = Classifier::train(&training, DEFAULT_EPOCHS, DEFAULT_RATE);
            let seconds = started.elapsed().as_secs_f32();

            let compact = model.to_compact();
            let json = match compact.to_json() {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("could not serialise the model: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = std::fs::write(out, &json) {
                eprintln!("could not write {out}: {e}");
                return ExitCode::FAILURE;
            }

            println!("{} sentences, trained in {seconds:.2}s", training.len());
            println!("training accuracy {:.3}", model.accuracy(&training));
            println!("holdout accuracy  {:.3}", model.accuracy(&corpus::holdout()));
            println!(
                "wrote {out} ({:.0} KB on disk, {:.0} KB of weights at 8 bits)",
                json.len() as f32 / 1024.0,
                compact.size_bytes() as f32 / 1024.0
            );
            ExitCode::SUCCESS
        }

        "accuracy" => {
            let world = World::default();
            let model = load_or_train();
            println!("training {:.3}", model.accuracy(&corpus::build(&world)));
            println!("holdout  {:.3}", model.accuracy(&corpus::holdout()));
            println!("weights  {:.0} KB", model.size_bytes() as f32 / 1024.0);
            ExitCode::SUCCESS
        }

        _ => usage(),
    }
}
