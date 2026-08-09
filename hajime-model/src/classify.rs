//! The learned part: a linear classifier, trained here, from nothing.
//!
//! Multinomial logistic regression over the hashed n-grams from [`features`].
//! One weight vector per intent, softmax across them, trained by gradient
//! descent. The arithmetic is all in this file; there is no framework under it
//! and no weights downloaded from anywhere.
//!
//! Why a linear model and not something deeper. The task is to separate a dozen
//! intents whose vocabularies barely overlap, from a few thousand examples. A
//! linear model solves that to the ceiling the data allows, trains in under a
//! second on the CPU this machine has, and holds still in a megabyte. A deeper
//! network on this data would spend more memory to memorise the same corpus and
//! be harder to explain when it got something wrong. The honest limit is that
//! it cannot learn word order: "stop after starting caddy" and "start after
//! stopping caddy" look alike to it. Nothing in the command surface depends on
//! that distinction, and [`plan`] asks rather than guesses when the margin is
//! thin.
//!
//! What it deliberately does not do is decide anything. It reports a label and
//! how sure it is. Acting on a low-confidence label is a decision, and it
//! belongs where the consequences are visible.

use crate::corpus::{Example, Intent};
use crate::features::{self, BUCKETS};
use serde::{Deserialize, Serialize};

/// How hard to shrink the weights each update.
///
/// Chosen by measurement rather than taste. The model reaches perfect training
/// accuracy at every setting tried, so the penalty is what buys anything on
/// sentences it has not seen: roughly 0.75 without it, 0.78 with it.
///
/// The first version of this number was 0.84, and it was wrong. Widening the
/// corpus had copied phrasings out of the held-out set into the training set,
/// so the model was being tested on sentences it had been trained on. The
/// exact-match test in `corpus.rs` caught one; the near-duplicates had to be
/// removed by hand. The lesson is in that test, which is why it is there.
///
/// `examples/sweep.rs` produced these numbers and is the thing to re-run
/// before changing this.
pub const DEFAULT_DECAY: f32 = 1e-3;

/// Training settings the sweep found best, which are also the cheapest it
/// tried. More epochs bought nothing measurable.
pub const DEFAULT_EPOCHS: usize = 20;
pub const DEFAULT_RATE: f32 = 1.0;

/// A trained model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classifier {
    /// One row of weights per intent, each `BUCKETS` long.
    weights: Vec<Vec<f32>>,
    bias: Vec<f32>,
    /// Stored so a model trained against a different feature space is rejected
    /// on load rather than silently producing nonsense.
    buckets: usize,
    classes: usize,
}

/// What the classifier thinks, and how strongly.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub intent: Intent,
    /// Softmax probability of the winning class, in 0..1.
    pub confidence: f32,
    /// The gap to the runner-up. A high confidence with a thin margin means two
    /// intents both fit, which is a different situation from one clear answer
    /// and is worth acting on differently.
    pub margin: f32,
    pub runner_up: Intent,
}

impl Classifier {
    fn blank(classes: usize) -> Self {
        Self {
            weights: vec![vec![0.0; BUCKETS]; classes],
            bias: vec![0.0; classes],
            buckets: BUCKETS,
            classes,
        }
    }

    /// Train on the corpus.
    ///
    /// Stochastic gradient descent: one update per example rather than one per
    /// epoch. The first version of this accumulated over the whole corpus and
    /// divided the step by the number of examples, which on two thousand
    /// sentences made each epoch move the weights by almost nothing. It ran,
    /// reported no error, and could not fit its own training set.
    ///
    /// The order is shuffled each epoch, because the corpus is generated intent
    /// by intent: walking it in order means hundreds of consecutive `Start`
    /// updates, and the model spends each block unlearning the last one. The
    /// shuffle uses a fixed seed so two runs on the same corpus produce the
    /// same weights, which is what makes a regression in accuracy attributable
    /// to a change rather than to luck.
    pub fn train(examples: &[Example], epochs: usize, learning_rate: f32) -> Self {
        Self::train_with_decay(examples, epochs, learning_rate, DEFAULT_DECAY)
    }

    /// Train with an explicit L2 penalty.
    ///
    /// The model reaches perfect training accuracy at every setting tried, so
    /// what limits it is not fitting but generalising. Shrinking the weights a
    /// little on each update discourages it from leaning hard on a single
    /// n-gram that happened to appear in one phrasing.
    pub fn train_with_decay(
        examples: &[Example],
        epochs: usize,
        learning_rate: f32,
        decay: f32,
    ) -> Self {
        let classes = Intent::ALL.len();
        let mut model = Self::blank(classes);

        // Extracted once. Re-extracting inside the epoch loop is the single
        // easiest way to make this slow for no reason.
        let prepared: Vec<(Vec<(usize, f32)>, usize)> = examples
            .iter()
            .map(|e| (features::extract(&e.text), e.intent.index()))
            .collect();
        if prepared.is_empty() {
            return model;
        }

        let mut order: Vec<usize> = (0..prepared.len()).collect();
        let mut rng = Xorshift::seeded(0x5EED_1234);

        for epoch in 0..epochs {
            rng.shuffle(&mut order);

            // Decay, so early epochs move fast and later ones settle instead of
            // oscillating around the minimum.
            let rate = learning_rate / (1.0 + epoch as f32 * 0.1);

            for &i in &order {
                let (feats, label) = &prepared[i];
                let probs = model.probabilities(feats);
                for (c, p) in probs.iter().enumerate() {
                    // The gradient of cross-entropy through a softmax is
                    // predicted minus actual, which is why no derivative
                    // appears here.
                    let error = p - if c == *label { 1.0 } else { 0.0 };
                    if error.abs() < 1e-6 {
                        continue;
                    }
                    let step = rate * error;
                    for (bucket, value) in feats {
                        let w = &mut model.weights[c][*bucket];
                        // Shrink toward zero as well as stepping down the
                        // gradient. Applied only to the buckets this sentence
                        // touches: sweeping all 4096 per example would cost
                        // more than the whole rest of training.
                        *w -= step * value + rate * decay * *w;
                    }
                    // The bias steps but is not decayed. Shrinking it only
                    // pushes the model toward predicting every class equally,
                    // which is not what the penalty is for.
                    model.bias[c] -= step;
                }
            }
        }

        model
    }

    fn scores(&self, feats: &[(usize, f32)]) -> Vec<f32> {
        (0..self.classes)
            .map(|c| {
                let mut sum = self.bias[c];
                for (bucket, value) in feats {
                    sum += self.weights[c][*bucket] * value;
                }
                sum
            })
            .collect()
    }

    fn probabilities(&self, feats: &[(usize, f32)]) -> Vec<f32> {
        let scores = self.scores(feats);
        // Subtract the maximum before exponentiating. Without it a confident
        // model overflows to infinity and every probability becomes NaN, which
        // shows up as the classifier suddenly answering at random.
        let max = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
        let total: f32 = exps.iter().sum();
        if total == 0.0 {
            return vec![1.0 / self.classes as f32; self.classes];
        }
        exps.into_iter().map(|e| e / total).collect()
    }

    /// Classify one sentence.
    ///
    /// `None` for text with no features at all: empty input, or punctuation on
    /// its own. Returning a guess there would be inventing an intent out of
    /// nothing.
    pub fn predict(&self, text: &str) -> Option<Prediction> {
        let feats = features::extract(text);
        if feats.is_empty() {
            return None;
        }

        let probs = self.probabilities(&feats);
        let mut ranked: Vec<(usize, f32)> = probs.into_iter().enumerate().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (best, best_p) = ranked[0];
        let (second, second_p) = ranked.get(1).copied().unwrap_or((best, 0.0));

        Some(Prediction {
            intent: Intent::from_index(best)?,
            confidence: best_p,
            margin: best_p - second_p,
            runner_up: Intent::from_index(second)?,
        })
    }

    /// Share of a set the model gets right. Used against the held-out
    /// phrasings, where it means something.
    pub fn accuracy(&self, examples: &[Example]) -> f32 {
        if examples.is_empty() {
            return 0.0;
        }
        let right = examples
            .iter()
            .filter(|e| self.predict(&e.text).map(|p| p.intent) == Some(e.intent))
            .count();
        right as f32 / examples.len() as f32
    }

    /// Roughly how much memory the weights occupy.
    pub fn size_bytes(&self) -> usize {
        (self.classes * self.buckets + self.classes) * std::mem::size_of::<f32>()
    }

    /// The weights at eight bits each, with one scale to bring them back.
    ///
    /// Measured, not assumed: at 8 bits the held-out accuracy is 0.778, the
    /// same as at 32. The decision is a comparison between sums, so rounding
    /// each weight moves every sum by a similar small amount and the ordering
    /// survives. `examples/shrink.rs` is where that was checked and is the
    /// thing to re-run before trusting it again.
    ///
    /// The table size is a separate question and the same experiment says it
    /// is already at its floor: halving the buckets costs accuracy at every
    /// step. Fewer bits per weight is free; fewer weights is not.
    pub fn to_compact(&self) -> Compact {
        let max = self
            .weights
            .iter()
            .flat_map(|row| row.iter())
            .fold(0f32, |m, w| m.max(w.abs()));
        // A table of zeros would divide by zero. It cannot classify anything
        // either, but it should round-trip rather than produce NaN.
        let scale = if max > 0.0 { 127.0 / max } else { 1.0 };

        Compact {
            weights: self
                .weights
                .iter()
                .map(|row| row.iter().map(|w| (w * scale).round() as i8).collect())
                .collect(),
            // The bias stays at full precision: there is one per class, so
            // eleven floats, and it shifts every score for that class at once.
            bias: self.bias.clone(),
            scale,
            buckets: self.buckets,
            classes: self.classes,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Load a saved model, refusing one built for a different feature space.
    pub fn from_json(raw: &str) -> Result<Self, ModelError> {
        let model: Self = serde_json::from_str(raw).map_err(ModelError::Unreadable)?;
        if model.buckets != BUCKETS {
            return Err(ModelError::WrongShape {
                found: model.buckets,
                expected: BUCKETS,
            });
        }
        if model.classes != Intent::ALL.len() {
            return Err(ModelError::WrongClasses {
                found: model.classes,
                expected: Intent::ALL.len(),
            });
        }
        Ok(model)
    }
}

/// A tiny deterministic generator, for shuffling only.
///
/// Not a random source anyone should trust for anything else, and not used for
/// anything else. It exists so training is reproducible: the same corpus gives
/// the same weights, so a drop in accuracy means somebody changed something.
struct Xorshift(u64);

impl Xorshift {
    fn seeded(seed: u64) -> Self {
        // Zero is a fixed point of xorshift and would emit zero forever.
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Fisher-Yates, walking backwards.
    fn shuffle(&mut self, items: &mut [usize]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// A trained model at eight bits per weight.
///
/// A quarter the size of the float form, and by measurement no less accurate
/// on this task. This is what gets written to disk and shipped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compact {
    weights: Vec<Vec<i8>>,
    bias: Vec<f32>,
    /// Multiply a stored weight by this to recover the original.
    scale: f32,
    buckets: usize,
    classes: usize,
}

impl Compact {
    pub fn size_bytes(&self) -> usize {
        self.classes * self.buckets + self.classes * std::mem::size_of::<f32>()
    }

    /// Back to the form that classifies.
    pub fn expand(&self) -> Classifier {
        Classifier {
            weights: self
                .weights
                .iter()
                .map(|row| row.iter().map(|w| *w as f32 / self.scale).collect())
                .collect(),
            bias: self.bias.clone(),
            buckets: self.buckets,
            classes: self.classes,
        }
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(raw: &str) -> Result<Self, ModelError> {
        let m: Self = serde_json::from_str(raw).map_err(ModelError::Unreadable)?;
        if m.buckets != BUCKETS {
            return Err(ModelError::WrongShape { found: m.buckets, expected: BUCKETS });
        }
        if m.classes != Intent::ALL.len() {
            return Err(ModelError::WrongClasses {
                found: m.classes,
                expected: Intent::ALL.len(),
            });
        }
        Ok(m)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("the saved model could not be read: {0}")]
    Unreadable(#[source] serde_json::Error),
    #[error("the saved model uses {found} feature buckets, this build uses {expected}")]
    WrongShape { found: usize, expected: usize },
    #[error("the saved model knows {found} intents, this build has {expected}")]
    WrongClasses { found: usize, expected: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus;
    use crate::world::World;

    fn trained() -> Classifier {
        Classifier::train(&corpus::build(&World::default()), DEFAULT_EPOCHS, DEFAULT_RATE)
    }

    #[test]
    fn it_learns_the_training_set() {
        // The floor. A model that cannot fit its own corpus has a bug in the
        // gradient, not a shortage of data.
        let model = trained();
        let acc = model.accuracy(&corpus::build(&World::default()));
        assert!(acc > 0.95, "training accuracy {acc:.2}");
    }

    #[test]
    fn it_generalises_to_phrasings_it_never_saw() {
        // The number that matters. The holdout shares no sentence with the
        // corpus and is written in a different register on purpose.
        let model = trained();
        let acc = model.accuracy(&corpus::holdout());
        assert!(
            acc > 0.75,
            "holdout accuracy {acc:.2}; the measured figure is 0.78, so this is a regression"
        );
    }

    #[test]
    fn it_works_in_arabic_and_english_alike() {
        let model = trained();
        let holdout = corpus::holdout();

        let arabic: Vec<&Example> = holdout
            .iter()
            .filter(|e| e.text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)))
            .collect();
        let english: Vec<&Example> = holdout
            .iter()
            .filter(|e| !e.text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)))
            .collect();

        let score = |set: &[&Example]| -> f32 {
            let right = set
                .iter()
                .filter(|e| model.predict(&e.text).map(|p| p.intent) == Some(e.intent))
                .count();
            right as f32 / set.len().max(1) as f32
        };

        let (a, e) = (score(&arabic), score(&english));
        assert!(a > 0.6, "Arabic accuracy {a:.2}");
        assert!(e > 0.6, "English accuracy {e:.2}");
    }

    #[test]
    fn the_clear_cases_are_answered_with_conviction() {
        let model = trained();
        for (text, want) in [
            ("stop caddy", Intent::Stop),
            ("أوقف كادي", Intent::Stop),
            ("start postgresql", Intent::Start),
            ("شغل بوستقرس", Intent::Start),
            ("what is running", Intent::Status),
            ("وضع التوفير", Intent::Save),
        ] {
            let p = model.predict(text).expect("real text has features");
            assert_eq!(p.intent, want, "{text:?} -> {:?}", p.intent);
            assert!(p.confidence > 0.4, "{text:?} confidence {:.2}", p.confidence);
        }
    }

    #[test]
    fn nonsense_does_not_come_back_certain() {
        // The property that keeps the planner honest: gibberish must not arrive
        // wearing the same confidence as a real instruction, or the threshold
        // downstream is meaningless.
        let model = trained();
        let sure = model.predict("stop caddy").unwrap().confidence;
        let vague = model.predict("qwerty zxcvb plugh xyzzy").unwrap().confidence;
        assert!(
            vague < sure,
            "gibberish scored {vague:.2} against {sure:.2} for a real instruction"
        );
    }

    #[test]
    fn empty_input_is_no_prediction_rather_than_a_guess() {
        let model = trained();
        assert!(model.predict("").is_none());
        assert!(model.predict("   ...  ").is_none());
    }

    #[test]
    fn probabilities_are_a_distribution() {
        let model = trained();
        let feats = features::extract("restart caddy");
        let probs = model.probabilities(&feats);
        let total: f32 = probs.iter().sum();
        assert!((total - 1.0).abs() < 1e-4, "sums to {total}");
        assert!(probs.iter().all(|p| p.is_finite()), "a NaN means the softmax overflowed");
        assert!(probs.iter().all(|&p| (0.0..=1.0).contains(&p)));
    }

    #[test]
    fn a_confident_model_does_not_overflow_the_softmax() {
        // Trained hard on purpose: without the max-subtraction in
        // `probabilities` this is where exp() reaches infinity and every
        // answer becomes NaN.
        let model = Classifier::train(&corpus::build(&World::default()), 200, 20.0);
        let p = model.predict("stop caddy").expect("features exist");
        assert!(p.confidence.is_finite(), "confidence is {}", p.confidence);
        assert!(p.margin.is_finite());
    }

    #[test]
    fn the_whole_model_is_small_enough_to_sit_beside_a_database() {
        let model = trained();
        let mb = model.size_bytes() as f32 / (1024.0 * 1024.0);
        assert!(mb < 1.0, "the weights occupy {mb:.2} MB");
    }

    #[test]
    fn a_saved_model_predicts_exactly_as_the_original_did() {
        let model = trained();
        let json = model.to_json().unwrap();
        let loaded = Classifier::from_json(&json).unwrap();
        for text in ["stop caddy", "شغل النموذج", "what is running"] {
            assert_eq!(model.predict(text), loaded.predict(text), "{text}");
        }
    }

    #[test]
    fn eight_bit_storage_answers_the_same_as_full_precision() {
        // The claim the compact form rests on, as a test rather than a note.
        // Measured at 0.778 either way in examples/shrink.rs; this asserts the
        // per-sentence agreement that number is made of.
        let full = trained();
        let small = full.to_compact().expand();

        let holdout = corpus::holdout();
        let mut agree = 0usize;
        for e in &holdout {
            if full.predict(&e.text).map(|p| p.intent)
                == small.predict(&e.text).map(|p| p.intent)
            {
                agree += 1;
            }
        }
        assert_eq!(
            agree,
            holdout.len(),
            "quantising changed {} of {} answers",
            holdout.len() - agree,
            holdout.len()
        );
        assert_eq!(small.accuracy(&holdout), full.accuracy(&holdout));
    }

    #[test]
    fn the_compact_form_is_a_quarter_the_size() {
        let full = trained();
        let small = full.to_compact();
        assert!(
            small.size_bytes() * 3 < full.size_bytes(),
            "{} against {}",
            small.size_bytes(),
            full.size_bytes()
        );
    }

    #[test]
    fn the_compact_form_round_trips_through_json() {
        let full = trained();
        let json = full.to_compact().to_json().unwrap();
        let back = Compact::from_json(&json).unwrap().expand();
        for text in ["stop caddy", "شغل النموذج", "what is running"] {
            assert_eq!(
                full.predict(text).map(|p| p.intent),
                back.predict(text).map(|p| p.intent),
                "{text}"
            );
        }
    }

    #[test]
    fn an_all_zero_model_quantises_without_dividing_by_zero() {
        // It cannot classify anything, but it must round-trip rather than
        // produce NaN and answer at random.
        let blank = Classifier::blank(Intent::ALL.len());
        let back = blank.to_compact().expand();
        let p = back.predict("stop caddy").expect("features exist");
        assert!(p.confidence.is_finite(), "confidence is {}", p.confidence);
    }

    #[test]
    fn a_model_from_a_different_feature_space_is_refused_not_used() {
        // Loading it anyway would map every feature to the wrong weight, and
        // the only symptom would be bad answers.
        let mut model = trained();
        model.buckets = BUCKETS * 2;
        let json = serde_json::to_string(&model).unwrap();
        match Classifier::from_json(&json) {
            Err(ModelError::WrongShape { .. }) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
