//! Which language the console answers in.
//!
//! The rule this follows is the one every other system follows: the language is
//! a property of whoever is reading, not of the page. It comes from the request
//! -- `?lang=` when someone has chosen, `Accept-Language` when they have not --
//! and it decides the text, the writing direction and the alignment together.
//!
//! What is deliberately *not* translated: the sentences that explain a failure.
//! Those quote what a service said and are the strings a person pastes into a
//! search box or an issue when the machine is broken at two in the morning.
//! Translating them would make the console friendlier and the search results
//! empty.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Lang {
    #[default]
    En,
    Ar,
}

impl Lang {
    /// `?lang=ar` wins, because it is a choice; the browser's list is a default.
    pub fn from_request(query: Option<&str>, accept_language: Option<&str>) -> Self {
        if let Some(q) = query {
            for pair in q.split('&') {
                if let Some(value) = pair.strip_prefix("lang=") {
                    return match value {
                        "ar" => Lang::Ar,
                        _ => Lang::En,
                    };
                }
            }
        }
        // Not a full RFC 4647 match: the console has two languages, so the
        // question is only whether Arabic is asked for before English is.
        if let Some(header) = accept_language {
            for tag in header.split(',') {
                let tag = tag.trim().split(';').next().unwrap_or("").to_ascii_lowercase();
                if tag.starts_with("ar") {
                    return Lang::Ar;
                }
                if tag.starts_with("en") {
                    return Lang::En;
                }
            }
        }
        Lang::En
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ar => "ar",
        }
    }

    pub fn dir(self) -> &'static str {
        match self {
            Lang::En => "ltr",
            Lang::Ar => "rtl",
        }
    }

    /// The other one, for the switch in the corner.
    pub fn other(self) -> Self {
        match self {
            Lang::En => Lang::Ar,
            Lang::Ar => Lang::En,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ar => "العربية",
        }
    }

    pub fn t(self, key: Key) -> &'static str {
        match (self, key) {
            (Lang::En, Key::Services) => "services",
            (Lang::Ar, Key::Services) => "الخدمات",
            (Lang::En, Key::RecentRuns) => "recent runs",
            (Lang::Ar, Key::RecentRuns) => "آخر التشغيلات",
            (Lang::En, Key::ModelDid) => "what the model did",
            (Lang::Ar, Key::ModelDid) => "ما فعله النموذج",
            (Lang::En, Key::ModelView) => "the model's view",
            (Lang::Ar, Key::ModelView) => "رأي النموذج",
            (Lang::En, Key::GoBackTo) => "you can go back to",
            (Lang::Ar, Key::GoBackTo) => "يمكنك الرجوع إلى",

            (Lang::En, Key::Workflow) => "workflow",
            (Lang::Ar, Key::Workflow) => "الوركفلو",
            (Lang::En, Key::Trigger) => "trigger",
            (Lang::Ar, Key::Trigger) => "المُشغِّل",
            (Lang::En, Key::Took) => "took",
            (Lang::Ar, Key::Took) => "استغرق",
            (Lang::En, Key::Result) => "result",
            (Lang::Ar, Key::Result) => "النتيجة",
            (Lang::En, Key::Why) => "why",
            (Lang::Ar, Key::Why) => "السبب",
            (Lang::En, Key::Tool) => "tool",
            (Lang::Ar, Key::Tool) => "الأداة",
            (Lang::En, Key::Caller) => "caller",
            (Lang::Ar, Key::Caller) => "المُنادي",
            (Lang::En, Key::Effect) => "effect",
            (Lang::Ar, Key::Effect) => "الأثر",

            (Lang::En, Key::NoRuns) => "no runs recorded yet",
            (Lang::Ar, Key::NoRuns) => "لا تشغيلات مسجّلة بعد",
            (Lang::En, Key::NoTools) => "the model has not used a tool yet",
            (Lang::Ar, Key::NoTools) => "لم يستخدم النموذج أداة بعد",
            (Lang::En, Key::AllSatisfied) => "every constraint is satisfied",
            (Lang::Ar, Key::AllSatisfied) => "كل القيود مستوفاة",

            (Lang::En, Key::Refreshes) => "refreshes every 30 seconds",
            (Lang::Ar, Key::Refreshes) => "يتحدّث كل ثلاثين ثانية",
            (Lang::En, Key::BuiltBy) => "built by",
            (Lang::Ar, Key::BuiltBy) => "بناه",
        }
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Services,
    RecentRuns,
    ModelDid,
    ModelView,
    GoBackTo,
    Workflow,
    Trigger,
    Took,
    Result,
    Why,
    Tool,
    Caller,
    Effect,
    NoRuns,
    NoTools,
    AllSatisfied,
    Refreshes,
    BuiltBy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chosen_language_beats_the_browsers_list() {
        // Someone who clicked the switch has said what they want; the header is
        // what their browser guessed months ago.
        let l = Lang::from_request(Some("lang=ar"), Some("en-GB,en;q=0.9"));
        assert_eq!(l, Lang::Ar);
    }

    #[test]
    fn arabic_is_taken_from_the_header_when_nothing_was_chosen() {
        let l = Lang::from_request(None, Some("ar-SA,ar;q=0.9,en;q=0.8"));
        assert_eq!(l, Lang::Ar);
    }

    #[test]
    fn the_first_language_the_reader_asked_for_wins() {
        // A list is in preference order. Reading it as a set and looking for
        // Arabic anywhere would give Arabic to someone who put English first.
        let l = Lang::from_request(None, Some("en-US,en;q=0.9,ar;q=0.5"));
        assert_eq!(l, Lang::En);
    }

    #[test]
    fn no_signal_at_all_is_english() {
        assert_eq!(Lang::from_request(None, None), Lang::En);
    }

    #[test]
    fn an_unknown_language_is_not_an_error() {
        assert_eq!(Lang::from_request(Some("lang=fr"), None), Lang::En);
        assert_eq!(Lang::from_request(None, Some("fr-FR,fr;q=0.9")), Lang::En);
    }

    #[test]
    fn direction_follows_the_language() {
        assert_eq!(Lang::Ar.dir(), "rtl");
        assert_eq!(Lang::En.dir(), "ltr");
    }

    #[test]
    fn every_key_has_both_languages() {
        // The match is exhaustive, so this cannot fail to compile with a key
        // missing. What it can do is return an empty string, which renders as a
        // blank panel heading and looks like a bug in the collector.
        for key in [
            Key::Services, Key::RecentRuns, Key::ModelDid, Key::ModelView,
            Key::GoBackTo, Key::Workflow, Key::Trigger, Key::Took, Key::Result,
            Key::Why, Key::Tool, Key::Caller, Key::Effect, Key::NoRuns,
            Key::NoTools, Key::AllSatisfied, Key::Refreshes, Key::BuiltBy,
        ] {
            assert!(!Lang::En.t(key).is_empty(), "{key:?} has no English");
            assert!(!Lang::Ar.t(key).is_empty(), "{key:?} has no Arabic");
        }
    }
}
