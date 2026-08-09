//! Pick the training settings by measuring, not by taste.
//!
//! Run with: cargo run -p hajime-model --example sweep --release
//!
//! Reports held-out accuracy and training time for a grid of epochs and
//! learning rates. The defaults in `train` should be whatever this says, and
//! when they are changed this is the thing to re-run.

use hajime_model::classify::Classifier;
use hajime_model::corpus;
use hajime_model::world::World;

fn main() {
    let world = World::default();
    let training = corpus::build(&world);
    let holdout = corpus::holdout();

    println!("corpus {} sentences, holdout {}", training.len(), holdout.len());
    println!();
    println!("{:>7} {:>6} {:>8} {:>9} {:>9} {:>8}", "epochs", "rate", "decay", "train", "holdout", "seconds");

    let mut best = (0usize, 0.0f32, 0.0f32, 0.0f32);
    for epochs in [20usize, 40, 80] {
        for rate in [0.5f32, 1.0, 2.0] {
            for decay in [0.0f32, 1e-5, 1e-4, 1e-3] {
            let started = std::time::Instant::now();
            let model = Classifier::train_with_decay(&training, epochs, rate, decay);
            let seconds = started.elapsed().as_secs_f32();

            let train_acc = model.accuracy(&training);
            let hold_acc = model.accuracy(&holdout);
            println!(
                "{epochs:>7} {rate:>6.2} {decay:>8.0e} {train_acc:>9.3} {hold_acc:>9.3} {seconds:>8.2}"
            );

            // Held-out accuracy decides. Training accuracy above it only says
            // the model memorised more, which is not the same as knowing more.
            if hold_acc > best.3 {
                best = (epochs, rate, decay, hold_acc);
            }
            }
        }
    }

    println!();
    println!(
        "best: {} epochs, rate {:.2}, decay {:.0e}, holdout {:.3}",
        best.0, best.1, best.2, best.3
    );
}
