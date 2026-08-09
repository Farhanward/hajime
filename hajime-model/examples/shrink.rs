//! How small can the model get before it stops being right?
//!
//! Run with: cargo run -p hajime-model --example shrink --release
//!
//! The weight table is `BUCKETS * classes * 4` bytes, so the bucket count is
//! the whole size. A smaller table means more n-grams colliding into the same
//! weight; the question is at what point that starts costing answers.
//!
//! Measured rather than guessed, by folding the existing feature indices into a
//! smaller range. That is exactly what a smaller table would do, so the numbers
//! are the numbers, without recompiling once per candidate.

use hajime_model::classify::{Classifier, DEFAULT_EPOCHS, DEFAULT_RATE};
use hajime_model::corpus::{self, Example, Intent};
use hajime_model::features;
use hajime_model::world::World;

/// Train and score at a simulated table size.
///
/// A separate implementation from `Classifier` on purpose: this one takes the
/// bucket count as a parameter so every candidate is measured the same way.
struct Small {
    weights: Vec<Vec<f32>>,
    bias: Vec<f32>,
    buckets: usize,
}

impl Small {
    fn train(examples: &[Example], buckets: usize) -> Self {
        let classes = Intent::ALL.len();
        let mut m = Small {
            weights: vec![vec![0.0; buckets]; classes],
            bias: vec![0.0; classes],
            buckets,
        };

        let prepared: Vec<(Vec<(usize, f32)>, usize)> = examples
            .iter()
            .map(|e| (fold(&features::extract(&e.text), buckets), e.intent.index()))
            .collect();

        let mut order: Vec<usize> = (0..prepared.len()).collect();
        let mut seed: u64 = 0x5EED_1234;

        for epoch in 0..DEFAULT_EPOCHS {
            // The same fixed shuffle the real trainer uses, so a difference in
            // the numbers is the table size and not the ordering.
            for i in (1..order.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                order.swap(i, (seed % (i as u64 + 1)) as usize);
            }
            let rate = DEFAULT_RATE / (1.0 + epoch as f32 * 0.1);

            for &i in &order {
                let (feats, label) = &prepared[i];
                let probs = m.probabilities(feats);
                for (c, p) in probs.iter().enumerate() {
                    let error = p - if c == *label { 1.0 } else { 0.0 };
                    if error.abs() < 1e-6 {
                        continue;
                    }
                    let step = rate * error;
                    for (b, v) in feats {
                        let w = &mut m.weights[c][*b];
                        *w -= step * v + rate * 1e-3 * *w;
                    }
                    m.bias[c] -= step;
                }
            }
        }
        m
    }

    fn probabilities(&self, feats: &[(usize, f32)]) -> Vec<f32> {
        let scores: Vec<f32> = (0..self.bias.len())
            .map(|c| {
                let mut sum = self.bias[c];
                for (b, v) in feats {
                    sum += self.weights[c][*b] * v;
                }
                sum
            })
            .collect();
        let max = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
        let total: f32 = exps.iter().sum();
        exps.into_iter().map(|e| e / total).collect()
    }

    fn accuracy(&self, examples: &[Example]) -> f32 {
        let right = examples
            .iter()
            .filter(|e| {
                let f = fold(&features::extract(&e.text), self.buckets);
                if f.is_empty() {
                    return false;
                }
                let p = self.probabilities(&f);
                let best = p
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                Intent::from_index(best) == Some(e.intent)
            })
            .count();
        right as f32 / examples.len().max(1) as f32
    }

    fn bytes(&self) -> usize {
        (self.buckets * self.bias.len() + self.bias.len()) * 4
    }
}

/// Fold feature indices into a smaller table, which is what a smaller table
/// does. Collisions are summed, as they would be.
fn fold(feats: &[(usize, f32)], buckets: usize) -> Vec<(usize, f32)> {
    let mut acc = vec![0f32; buckets];
    for (b, v) in feats {
        acc[b % buckets] += v;
    }
    acc.into_iter()
        .enumerate()
        .filter(|(_, v)| *v != 0.0)
        .collect()
}

fn main() {
    let world = World::default();
    let training = corpus::build(&world);
    let holdout = corpus::holdout();

    println!("corpus {} sentences, holdout {}", training.len(), holdout.len());
    println!();
    println!("{:>8} {:>10} {:>9} {:>9}", "buckets", "size", "train", "holdout");

    let full = Classifier::train(&training, DEFAULT_EPOCHS, DEFAULT_RATE);
    println!(
        "{:>8} {:>9}K {:>9.3} {:>9.3}   <- current",
        features::BUCKETS,
        full.size_bytes() / 1024,
        full.accuracy(&training),
        full.accuracy(&holdout),
    );

    let mut best: Option<(usize, f32, usize)> = None;
    for buckets in [2048usize, 1024, 512, 256, 128, 64] {
        let m = Small::train(&training, buckets);
        let train = m.accuracy(&training);
        let hold = m.accuracy(&holdout);
        println!(
            "{:>8} {:>9}K {:>9.3} {:>9.3}",
            buckets,
            m.bytes() / 1024,
            train,
            hold
        );
        // The smallest table that keeps the measured holdout. Not the best
        // score: a table that scores higher by luck on 45 sentences is not
        // better, and the brief was smaller.
        if hold >= 0.75 {
            best = Some((buckets, hold, m.bytes()));
        }
    }

    // --- the other axis: fewer bits per weight, not fewer weights ----------
    //
    // The table size is capacity and the measurements above say it is already
    // at the floor. Precision is separate: the decision is a comparison
    // between sums, so small rounding in each weight moves every sum a little
    // and the ordering usually survives.
    println!();
    println!("{:>8} {:>10} {:>9} {:>9}", "bits", "size", "train", "holdout");
    println!(
        "{:>8} {:>9}K {:>9.3} {:>9.3}   <- current",
        32,
        full.size_bytes() / 1024,
        full.accuracy(&training),
        full.accuracy(&holdout),
    );

    for bits in [16u32, 8] {
        let q = quantised(&full, &training, bits);
        println!(
            "{:>8} {:>9}K {:>9.3} {:>9.3}",
            bits,
            (features::BUCKETS * Intent::ALL.len() * (bits as usize / 8)) / 1024,
            q.0,
            q.1
        );
    }

    println!();
    match best {
        Some((b, h, bytes)) => println!(
            "smallest table holding 0.75 or better: {b} buckets, {}K, holdout {h:.3}",
            bytes / 1024
        ),
        None => println!("nothing below the current size holds the accuracy"),
    }
}

/// Round every weight to `bits` and score the result.
///
/// Symmetric, one scale for the whole table: the largest magnitude maps to the
/// top of the range and everything else in proportion. Per-class scales would
/// be a little more accurate and a lot more to get wrong on the way back in.
fn quantised(model: &Classifier, training: &[Example], bits: u32) -> (f32, f32) {
    let json = model.to_json().expect("a trained model serialises");
    let raw: serde_json::Value = serde_json::from_str(&json).unwrap();

    let weights: Vec<Vec<f32>> = serde_json::from_value(raw["weights"].clone()).unwrap();
    let bias: Vec<f32> = serde_json::from_value(raw["bias"].clone()).unwrap();

    let max = weights
        .iter()
        .flat_map(|row| row.iter())
        .fold(0f32, |m, w| m.max(w.abs()));
    let levels = ((1u32 << (bits - 1)) - 1) as f32;
    let scale = if max > 0.0 { levels / max } else { 1.0 };

    let rounded: Vec<Vec<f32>> = weights
        .iter()
        .map(|row| row.iter().map(|w| (w * scale).round() / scale).collect())
        .collect();

    let score = |set: &[Example]| -> f32 {
        let right = set
            .iter()
            .filter(|e| {
                let feats = features::extract(&e.text);
                if feats.is_empty() {
                    return false;
                }
                let scores: Vec<f32> = (0..bias.len())
                    .map(|c| {
                        let mut sum = bias[c];
                        for (b, v) in &feats {
                            sum += rounded[c][*b] * v;
                        }
                        sum
                    })
                    .collect();
                let best = scores
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                Intent::from_index(best) == Some(e.intent)
            })
            .count();
        right as f32 / set.len().max(1) as f32
    };

    (score(training), score(&corpus::holdout()))
}
