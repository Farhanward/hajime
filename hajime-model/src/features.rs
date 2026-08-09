//! Turning a sentence into numbers, without a tokenizer.
//!
//! Character n-grams rather than words. Two reasons, and both are about the
//! person typing.
//!
//! Arabic attaches its articles and pronouns to the word: «الخدمة» and «خدمة»
//! and «للخدمة» are one word to a reader and three to a word splitter, which
//! then learns each separately from a third of the examples. Character n-grams
//! see the shared «خدم» and treat them as the same thing, which is what they
//! are.
//!
//! And people type both languages in one sentence here, often in one clause. A
//! word tokenizer needs to know which language it is looking at before it can
//! split; an n-gram does not need to know at all.
//!
//! Features are hashed into a fixed table rather than kept in a dictionary. The
//! model is then a fixed-size array of floats whatever it was trained on, which
//! is what lets it sit in memory next to a database without anyone noticing.

/// Size of the hashed feature space.
///
/// Large enough that collisions between the few thousand n-grams this domain
/// produces are rare, small enough that the whole model is under a megabyte per
/// class.
pub const BUCKETS: usize = 4096;

/// Normalise before extracting.
///
/// Arabic is written with and without diacritics and with several shapes of the
/// same letter, and the same person will type it both ways in one session.
/// Folding those together means the model learns the word, not the keyboard.
/// Stands for "remove this character entirely". A control character, so it can
/// never collide with anything a person types.
const DROP: char = '\u{0000}';

pub fn normalise(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let c = match c {
            // Alef in its several written forms.
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            // Taa marbuta is routinely typed as haa.
            'ة' => 'ه',
            // Alef maqsura for yaa, the most common Arabic typing variation.
            'ى' => 'ي',
            // Hamza sitting on a chair. Leaving these alone was a real defect:
            // «اطفئ» and «اطفي» are the same word and the same request, and
            // without the fold the model learned whichever spelling happened to
            // be in the corpus and read the other as a different intent.
            'ئ' => 'ي',
            'ؤ' => 'و',
            // A bare hamza carries nothing once the chairs are folded. Mapped
            // to a marker the loop below drops rather than to a letter.
            'ء' => DROP,
            // Arabic-Indic digits, which arrive from phone keyboards.
            '٠' => '0', '١' => '1', '٢' => '2', '٣' => '3', '٤' => '4',
            '٥' => '5', '٦' => '6', '٧' => '7', '٨' => '8', '٩' => '9',
            other => other,
        };

        // Drop the diacritics entirely: they are optional in writing, so their
        // presence carries no information about intent.
        if is_arabic_diacritic(c) || c == DROP {
            continue;
        }

        if c.is_alphanumeric() {
            for lower in c.to_lowercase() {
                out.push(lower);
            }
        } else if !out.ends_with(' ') {
            // Any run of punctuation or space collapses to one separator, so
            // "restart  the-service!" and "restart the service" agree.
            out.push(' ');
        }
    }
    out.trim().to_string()
}

fn is_arabic_diacritic(c: char) -> bool {
    matches!(c, '\u{064B}'..='\u{0652}' | '\u{0670}' | '\u{0640}')
}

/// A stable hash. Not the standard library's, deliberately.
///
/// `DefaultHasher` is explicitly allowed to change between releases. A model
/// trained today and loaded after a toolchain upgrade would then map every
/// feature to a different bucket and quietly answer nonsense, with nothing in
/// the output to suggest why. FNV-1a is fixed for as long as this file says so.
fn hash(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The features of one sentence, as (bucket, count) pairs.
///
/// Padded with a leading and trailing space so the first and last characters
/// of a word carry the same weight as the middle ones. Without the padding a
/// three-gram never sees a word's opening letter in initial position, which is
/// exactly where Arabic puts its prefixes.
pub fn extract(text: &str) -> Vec<(usize, f32)> {
    let norm = format!(" {} ", normalise(text));
    let chars: Vec<char> = norm.chars().collect();

    let mut counts = vec![0f32; BUCKETS];

    // Three and four together: three catches the shared root, four separates
    // words that share one.
    for n in [3usize, 4] {
        if chars.len() < n {
            continue;
        }
        for window in chars.windows(n) {
            let gram: String = window.iter().collect();
            let bucket = (hash(gram.as_bytes()) as usize) % BUCKETS;
            counts[bucket] += 1.0;
        }
    }

    // L2 normalisation, so a long sentence does not simply outvote a short one.
    // Without it "stop" and a paragraph ending in "stop" get very different
    // scores for the same intent.
    let norm_factor: f32 = counts.iter().map(|c| c * c).sum::<f32>().sqrt();
    let scale = if norm_factor > 0.0 { 1.0 / norm_factor } else { 0.0 };

    counts
        .into_iter()
        .enumerate()
        .filter(|(_, c)| *c > 0.0)
        .map(|(i, c)| (i, c * scale))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_spelling_variants_fold_together() {
        // The same request typed by the same person on two keyboards.
        assert_eq!(normalise("أوقف الخدمة"), normalise("اوقف الخدمه"));
        assert_eq!(normalise("علي"), normalise("على"));
    }

    #[test]
    fn diacritics_are_dropped_because_they_are_optional_in_writing() {
        assert_eq!(normalise("شَغِّل"), normalise("شغل"));
    }

    #[test]
    fn punctuation_and_spacing_do_not_change_the_features() {
        assert_eq!(normalise("restart  the-service!!"), "restart the service");
    }

    #[test]
    fn case_is_folded() {
        assert_eq!(normalise("STOP Caddy"), "stop caddy");
    }

    #[test]
    fn arabic_indic_digits_become_ascii() {
        assert_eq!(normalise("المنفذ ٥٤٣٢"), "المنفذ 5432");
    }

    #[test]
    fn the_hash_is_pinned_so_a_trained_model_survives_a_toolchain_upgrade() {
        // If this value ever changes, every model trained before the change
        // maps its features to different buckets and answers nonsense. That is
        // why the hash is written out here rather than taken from std.
        assert_eq!(hash(b"stop"), 0xcb532ae5, "FNV-1a of the four bytes of \"stop\"");
    }

    #[test]
    fn features_are_normalised_to_unit_length() {
        let f = extract("stop caddy");
        let magnitude: f32 = f.iter().map(|(_, v)| v * v).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-5, "got {magnitude}");
    }

    #[test]
    fn a_long_sentence_does_not_outweigh_a_short_one() {
        let short = extract("stop caddy");
        let long = extract(
            "please could you go ahead and stop caddy for me when you get a chance",
        );
        let a: f32 = short.iter().map(|(_, v)| v * v).sum::<f32>().sqrt();
        let b: f32 = long.iter().map(|(_, v)| v * v).sum::<f32>().sqrt();
        assert!((a - b).abs() < 1e-5, "both should be unit length: {a} vs {b}");
    }

    #[test]
    fn every_bucket_is_in_range() {
        for text in ["stop caddy", "أوقف كادي", "restart hajime_workflow", ""] {
            for (bucket, _) in extract(text) {
                assert!(bucket < BUCKETS, "{bucket} out of range for {text:?}");
            }
        }
    }

    #[test]
    fn an_empty_sentence_produces_no_features_rather_than_a_panic() {
        assert!(extract("").is_empty());
        assert!(extract("   !!!   ").is_empty());
    }

    #[test]
    fn similar_sentences_share_most_of_their_features() {
        let a: Vec<usize> = extract("stop caddy").into_iter().map(|(i, _)| i).collect();
        let b: Vec<usize> = extract("stop caddy now").into_iter().map(|(i, _)| i).collect();
        let shared = a.iter().filter(|i| b.contains(i)).count();
        assert!(shared > a.len() / 2, "{shared} of {} shared", a.len());
    }
}
