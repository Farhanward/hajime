//! What the model gets wrong, and how sure it was about it.
//!
//! Run with: cargo run -p hajime-model --example diagnose
//!
//! A single accuracy number tells you something is wrong and nothing about
//! what. This prints each held-out sentence the model misses, what it thought
//! instead, and its confidence, which is what says whether the fix is more
//! training data or a different threshold.

use hajime_model::classify::Classifier;
use hajime_model::corpus;
use hajime_model::world::World;

fn main() {
    let world = World::default();
    let training = corpus::build(&world);

    let started = std::time::Instant::now();
    let model = Classifier::train(&training, hajime_model::classify::DEFAULT_EPOCHS, hajime_model::classify::DEFAULT_RATE);
    let elapsed = started.elapsed();

    println!("corpus     {} sentences", training.len());
    println!("trained in {:.2}s", elapsed.as_secs_f32());
    println!("weights    {:.0} KB", model.size_bytes() as f32 / 1024.0);
    println!();

    let holdout = corpus::holdout();
    println!("training accuracy {:.3}", model.accuracy(&training));
    println!("holdout accuracy  {:.3}", model.accuracy(&holdout));
    println!();

    println!("misses:");
    let mut misses = 0;
    for example in &holdout {
        let Some(p) = model.predict(&example.text) else {
            println!("  (no features) {}", example.text);
            misses += 1;
            continue;
        };
        if p.intent != example.intent {
            misses += 1;
            println!(
                "  want {:<9} got {:<9} conf {:.2} margin {:.2}  {}",
                example.intent.as_str(),
                p.intent.as_str(),
                p.confidence,
                p.margin,
                example.text
            );
        }
    }
    if misses == 0 {
        println!("  none");
    }

    println!();
    println!("confidence on the misses versus the hits:");
    let (mut hit_conf, mut hits) = (0.0f32, 0usize);
    let (mut miss_conf, mut miss_n) = (0.0f32, 0usize);
    for example in &holdout {
        if let Some(p) = model.predict(&example.text) {
            if p.intent == example.intent {
                hit_conf += p.confidence;
                hits += 1;
            } else {
                miss_conf += p.confidence;
                miss_n += 1;
            }
        }
    }
    if hits > 0 {
        println!("  hits  {:.2} average over {hits}", hit_conf / hits as f32);
    }
    if miss_n > 0 {
        println!("  misses {:.2} average over {miss_n}", miss_conf / miss_n as f32);
        println!();
        println!("If the misses sit below the hits, the threshold in plan.rs turns");
        println!("them into questions rather than wrong actions, which is the");
        println!("behaviour that matters.");
    }
}
