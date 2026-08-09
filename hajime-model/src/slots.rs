//! Finding which service or jail a sentence is about.
//!
//! Not learned. The set of names is closed and known, so matching against it is
//! exact, and exactness here is a safety property rather than a shortcut: there
//! is no sequence of characters anyone can type that makes this function return
//! a service that is not in the table. A learned extractor would have some
//! probability, however small, of producing a plausible name that does not
//! exist, and the command built from it would fail somewhere less obvious.
//!
//! What it does tolerate is how people actually write the names: `hajime_wa`
//! typed as "hajime wa", postgres for postgresql, «كادي» for caddy. Those are
//! spelled out below rather than guessed.

use crate::features::normalise;
use crate::world::World;

/// A name found in the text.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub name: &'static str,
    /// How it appeared, for quoting back to the person who typed it.
    pub as_written: String,
    pub is_jail: bool,
}

/// Spellings that are not the canonical name.
///
/// Written out because each one is a decision. `postgres` is what everybody
/// calls it and `postgresql` is what the rc script is called; the Arabic
/// spellings are transliterations that vary by writer, so the common ones are
/// listed rather than derived.
const ALIASES: &[(&str, &str)] = &[
    ("postgres", "postgresql"),
    ("psql", "postgresql"),
    ("pg", "postgresql"),
    ("بوستقرس", "postgresql"),
    ("بوستجرس", "postgresql"),
    ("mariadb", "mysql"),
    ("maria", "mysql"),
    ("ماريا", "mysql"),
    ("مايسكل", "mysql"),
    ("كادي", "caddy"),
    ("ريديس", "redis"),
    ("تنل", "cloudflared"),
    ("tunnel", "cloudflared"),
    ("cf", "cloudflared"),
    ("النفق", "cloudflared"),
    ("workflow", "hajime_workflow"),
    ("workflows", "hajime_workflow"),
    ("الوركفلو", "hajime_workflow"),
    ("وركفلو", "hajime_workflow"),
    ("n8n", "hajime_workflow"),
    ("whatsapp", "hajime_wa"),
    ("wa", "hajime_wa"),
    ("واتساب", "hajime_wa"),
    ("الواتس", "hajime_wa"),
    ("bridge", "hajime_wa_bridge"),
    ("الجسر", "hajime_wa_bridge"),
    ("ai", "hajime_ai"),
    ("gateway", "hajime_ai"),
    ("البوابة", "hajime_ai"),
    ("llama", "llamacpp"),
    ("llamacpp", "llamacpp"),
    ("model", "llamacpp"),
    ("النموذج", "llamacpp"),
    ("موديل", "llamacpp"),
];

/// Every name in the text, in the order they appear.
///
/// Longest match first, so `hajime_wa_bridge` is never read as `hajime_wa`
/// followed by stray text. Getting that order wrong would send a stop command
/// to the gateway when the bridge was meant.
pub fn find(world: &World, text: &str) -> Vec<Found> {
    let haystack = normalise(text);

    // Canonical names, plus the aliases, sorted long to short.
    let mut candidates: Vec<(String, &'static str)> = Vec::new();
    for name in world.entity_names() {
        candidates.push((normalise(name), name));
        // `hajime_workflow` is typed as often with a space as with an
        // underscore, and the normaliser turns the underscore into one anyway.
        if name.contains('_') {
            candidates.push((normalise(&name.replace('_', " ")), name));
        }
    }
    for (alias, canonical) in ALIASES {
        // An alias for a name that no longer exists is dead weight and, worse,
        // would resolve to nothing at the point of use.
        if world.service(canonical).is_some() || world.jails().iter().any(|j| j.name == *canonical)
        {
            candidates.push((normalise(alias), canonical));
        }
    }
    // Longest first. Reversed rather than negated: the lengths are usize and
    // `std::cmp::Reverse` says what is meant without an underflow to think
    // about.
    candidates.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));

    let mut found: Vec<Found> = Vec::new();
    // Positions already consumed, so `hajime_wa_bridge` does not also report
    // the `hajime_wa` inside it.
    let mut taken: Vec<(usize, usize)> = Vec::new();

    for (needle, canonical) in &candidates {
        if needle.is_empty() {
            continue;
        }
        let mut from = 0usize;
        while let Some(offset) = haystack[from..].find(needle.as_str()) {
            let start = from + offset;
            let end = start + needle.len();
            from = end;

            if !on_word_boundary(&haystack, start, end) {
                continue;
            }
            if taken.iter().any(|(s, e)| start < *e && end > *s) {
                continue;
            }
            taken.push((start, end));
            if !found.iter().any(|f| f.name == *canonical) {
                found.push(Found {
                    name: canonical,
                    as_written: haystack[start..end].to_string(),
                    is_jail: world.jails().iter().any(|j| j.name == *canonical),
                });
            }
        }
    }

    // Report in the order they were written, which is the order they were
    // meant.
    found.sort_by_key(|f| haystack.find(&f.as_written).unwrap_or(usize::MAX));
    found
}

/// Arabic particles that attach to the front of the following word.
///
/// Arabic writes its article and several prepositions joined: «البوستقرس» is
/// "the postgres" as one word. Without this the matcher saw a name glued to a
/// longer token and refused it, so «اطفي البوستقرس» produced a confident `stop`
/// with nothing to stop. Longest first, since «وال» contains «ال».
const PROCLITICS: &[&str] = &["وبال", "فبال", "وال", "بال", "كال", "فال", "لل", "ال"];

/// Is this match a whole word rather than a fragment of a longer one?
///
/// Without this, "ai" matches inside "said" and "maintenance", and the model
/// starts confidently talking about the model gateway whenever anyone explains
/// something.
fn on_word_boundary(haystack: &str, start: usize, end: usize) -> bool {
    let after_ok = end >= haystack.len()
        || haystack[end..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true);
    if !after_ok {
        return false;
    }

    starts_a_word(haystack, start)
}

/// Does a word begin at `start`, allowing for an attached Arabic particle?
fn starts_a_word(haystack: &str, start: usize) -> bool {
    if start == 0 {
        return true;
    }
    let before = &haystack[..start];
    if before
        .chars()
        .next_back()
        .map(|c| !c.is_alphanumeric())
        .unwrap_or(true)
    {
        return true;
    }

    // The character before is a letter. That is still a word start if what sits
    // between it and the real boundary is one of the attached particles, and
    // only then: this must not turn into "any short prefix will do", or "ai"
    // starts matching inside every English word again.
    for particle in PROCLITICS {
        if let Some(rest) = before.strip_suffix(particle) {
            if rest.is_empty()
                || rest
                    .chars()
                    .next_back()
                    .map(|c| !c.is_alphanumeric())
                    .unwrap_or(true)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str) -> Vec<&'static str> {
        find(&World::default(), text).into_iter().map(|f| f.name).collect()
    }

    #[test]
    fn a_canonical_name_is_found() {
        assert_eq!(names("stop caddy"), vec!["caddy"]);
    }

    #[test]
    fn the_longest_name_wins() {
        // The failure this prevents: reading `hajime_wa_bridge` as the gateway
        // and stopping the wrong process.
        assert_eq!(names("restart hajime_wa_bridge"), vec!["hajime_wa_bridge"]);
    }

    #[test]
    fn underscores_and_spaces_are_the_same_name() {
        assert_eq!(names("start hajime workflow"), vec!["hajime_workflow"]);
        assert_eq!(names("start hajime_workflow"), vec!["hajime_workflow"]);
    }

    #[test]
    fn common_aliases_resolve_to_the_rc_name() {
        assert_eq!(names("stop postgres"), vec!["postgresql"]);
        assert_eq!(names("restart mariadb"), vec!["mysql"]);
        assert_eq!(names("stop n8n"), vec!["hajime_workflow"]);
    }

    #[test]
    fn arabic_spellings_resolve_too() {
        assert_eq!(names("أوقف كادي"), vec!["caddy"]);
        assert_eq!(names("شغل النموذج"), vec!["llamacpp"]);
        assert_eq!(names("اوقف الواتس"), vec!["hajime_wa"]);
    }

    #[test]
    fn a_name_inside_a_longer_word_is_not_a_match() {
        // "ai" lives inside "said", "maintenance" and "explain". Without the
        // boundary check the model brings up the gateway whenever someone
        // explains something.
        assert!(names("i said explain the maintenance").is_empty());
        assert!(names("please train the staff").is_empty());
    }

    #[test]
    fn several_names_come_back_in_the_order_written() {
        let n = names("stop caddy then stop redis");
        assert_eq!(n, vec!["caddy", "redis"]);
    }

    #[test]
    fn a_name_repeated_is_reported_once() {
        assert_eq!(names("restart caddy, yes caddy"), vec!["caddy"]);
    }

    #[test]
    fn jails_are_found_and_marked_as_jails() {
        let found = find(&World::default(), "roll back the ai jail");
        let ai = found.iter().find(|f| f.name == "ai").expect("the ai jail");
        assert!(ai.is_jail);
    }

    #[test]
    fn the_arabic_definite_article_does_not_hide_the_name() {
        // «البوستقرس» is one word meaning "the postgres". Refusing it produced
        // a confident `stop` with no service attached, which is the worst shape
        // of failure available here: the intent is right and the object is
        // missing.
        assert_eq!(names("ابي اطفي البوستقرس"), vec!["postgresql"]);
        assert_eq!(names("اوقف الكادي"), vec!["caddy"]);
        assert_eq!(names("وش يصير لو وقفت الماريا"), vec!["mysql"]);
    }

    #[test]
    fn attached_prepositions_do_not_hide_it_either() {
        assert_eq!(names("خذ لقطة للكادي"), vec!["caddy"]);
        assert_eq!(names("وش بالكادي"), vec!["caddy"]);
    }

    #[test]
    fn the_particle_rule_does_not_reopen_the_substring_hole() {
        // The rule allows a specific short list of attached particles, not any
        // short prefix. If it were loose, "ai" would start matching inside
        // English words again, which is the bug the boundary check exists for.
        assert!(names("i said explain the maintenance").is_empty());
        assert!(names("please train the staff").is_empty());
        assert!(names("the trailing detail").is_empty());
    }

    #[test]
    fn nothing_is_found_in_text_that_names_nothing() {
        assert!(names("what is running").is_empty());
        assert!(names("وش الوضع").is_empty());
    }

    #[test]
    fn no_input_can_produce_a_name_outside_the_table() {
        // The safety property of this module, stated as a test. Whatever is
        // typed, the output is drawn from the system's own vocabulary.
        let w = World::default();
        let vocabulary = w.entity_names();
        for text in [
            "stop postgresqlx",
            "start the frobnicator",
            "أوقف الخدمة الوهمية",
            "caddy2 redis9 hajime_nothing",
            "'; DROP TABLE services; --",
            "\u{0000}\u{FFFD} stop",
        ] {
            for found in find(&w, text) {
                assert!(
                    vocabulary.contains(&found.name),
                    "{text:?} produced {:?}, which is not a real entity",
                    found.name
                );
            }
        }
    }

    #[test]
    fn every_alias_points_at_something_that_exists() {
        // A dead alias resolves to a name the executor will then fail on.
        let w = World::default();
        for (alias, canonical) in ALIASES {
            let known = w.service(canonical).is_some()
                || w.jails().iter().any(|j| j.name == *canonical);
            assert!(known, "alias {alias:?} points at {canonical:?}, which does not exist");
        }
    }
}
