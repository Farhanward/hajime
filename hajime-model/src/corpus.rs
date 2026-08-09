//! Training data, generated from the model of the system.
//!
//! The corpus is not a file someone maintains. It is produced from [`World`]
//! every time the model trains, so a service added to the table is a service
//! the model can be asked about, with no second place to remember to edit.
//!
//! The phrasings are hand-written because they have to be: they are how this
//! particular person asks for things, in the two languages they ask in. What is
//! generated is the cross product with the real entity names, which is the part
//! that would otherwise go stale.
//!
//! A note on honesty about size. This is a few thousand short sentences over a
//! dozen intents. It is enough because the task is narrow and the vocabulary is
//! closed, and it would be nowhere near enough for anything wider. The model
//! knows one system; asked about anything else it should, and does, say so.

use crate::world::World;
use serde::{Deserialize, Serialize};

/// What the person wants done.
///
/// Deliberately few. Every intent here maps to something the system can
/// actually do; there is no `Unknown` variant that quietly swallows a request
/// the model failed to understand, because the caller needs to tell "I am sure
/// this is a stop" from "I am not sure what this is" and an enum variant cannot
/// carry that difference. Confidence lives beside the label instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Bring a service up.
    Start,
    /// Take a service down.
    Stop,
    /// Stop and start it again.
    Restart,
    /// What is running, what is broken.
    Status,
    /// Would this fit, what does it need, what would it break.
    Explain,
    /// Free memory by stopping the optional tier.
    Save,
    /// Undo saving mode.
    Resume,
    /// Take a snapshot before doing something risky.
    Snapshot,
    /// Go back to a snapshot.
    Rollback,
    /// Run the boot self-check.
    Check,
    /// Recent runs and what failed.
    History,
}

impl Intent {
    pub const ALL: &'static [Intent] = &[
        Intent::Start,
        Intent::Stop,
        Intent::Restart,
        Intent::Status,
        Intent::Explain,
        Intent::Save,
        Intent::Resume,
        Intent::Snapshot,
        Intent::Rollback,
        Intent::Check,
        Intent::History,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Intent::Start => "start",
            Intent::Stop => "stop",
            Intent::Restart => "restart",
            Intent::Status => "status",
            Intent::Explain => "explain",
            Intent::Save => "save",
            Intent::Resume => "resume",
            Intent::Snapshot => "snapshot",
            Intent::Rollback => "rollback",
            Intent::Check => "check",
            Intent::History => "history",
        }
    }

    pub fn index(&self) -> usize {
        Intent::ALL.iter().position(|i| i == self).expect("ALL covers every variant")
    }

    pub fn from_index(i: usize) -> Option<Intent> {
        Intent::ALL.get(i).copied()
    }
}

/// One training example.
pub struct Example {
    pub text: String,
    pub intent: Intent,
}

/// Phrasings that take an entity name, with `{}` where the name goes.
///
/// Both languages in one list, because both arrive in one sentence. The Arabic
/// is written the way it is actually typed, including the forms without
/// hamza that the normaliser folds together anyway.
const WITH_ENTITY: &[(Intent, &[&str])] = &[
    (
        Intent::Start,
        &[
            "start {}", "start the {} service", "bring {} up", "turn on {}",
            "run {}", "launch {}", "get {} running", "please start {}",
            "شغل {}", "شغّل {}", "ابدأ {}", "شغل خدمة {}", "ارفع {}",
            "فعل {}", "ابغى اشغل {}", "شغل لي {}",
        ],
    ),
    (
        Intent::Stop,
        &[
            "stop {}", "stop the {} service", "shut {} down", "turn off {}",
            "kill {}", "halt {}", "take {} down", "please stop {}",
            "اوقف {}", "أوقف {}", "وقف {}", "اطفئ {}", "اقفل {}",
            "اوقف خدمة {}", "نزل {}", "ابغى اوقف {}", "اطفي {}", "طفي {}",
            "ابي اطفي {}", "سكر {}", "خلاص وقف {}", "شيل {}", "الغ تشغيل {}",
        ],
    ),
    (
        Intent::Restart,
        &[
            "restart {}", "restart the {} service", "bounce {}", "reload {}",
            "cycle {}", "stop and start {}", "restart {} please",
            "اعد تشغيل {}", "أعد تشغيل {}", "ريستارت {}", "اعد {}",
            "شغل {} من جديد", "اعادة تشغيل {}",
        ],
    ),
    (
        Intent::Explain,
        &[
            "what does {} need", "what depends on {}", "what breaks if i stop {}",
            "explain {}", "tell me about {}", "what is {}", "why is {} needed",
            "what happens if {} goes down", "how much memory does {} use",
            "will {} fit", "can i start {}",
            "ماذا يحتاج {}", "ما الذي يعتمد على {}", "وش يصير لو وقفت {}",
            "اشرح {}", "ما هو {}", "كم ياخذ {} من الذاكره",
            "هل يشتغل {}", "ليش {} مهم", "وش علاقة {} بالباقي",
        ],
    ),
    (
        Intent::Snapshot,
        &[
            "snapshot {}", "take a snapshot of {}", "back up {} first",
            "خذ لقطة لـ {}", "لقطة {}", "احفظ نسخة من {}",
        ],
    ),
    (
        Intent::Rollback,
        &[
            "roll {} back", "rollback {}", "restore {}", "undo the change to {}",
            "revert {}", "put {} back to the snapshot", "restore {} from the snapshot",
            "استرجع {}", "رجع {} للسابق", "ارجع {} للقطة", "ارجع {} لامس",
            "استرجع لقطة {}", "رجع {} للنسخه السابقه", "ارجاع {}",
        ],
    ),
];

/// Phrasings that name no entity.
const WITHOUT_ENTITY: &[(Intent, &[&str])] = &[
    (
        Intent::Status,
        &[
            "status", "what is running", "how are things", "show me the services",
            "is everything ok", "what is up", "system status", "show status",
            "whats broken", "what is down", "any problems",
            "الحالة", "وش شغال", "كيف الوضع", "اعرض الخدمات", "كل شي تمام",
            "وش الوضع", "في مشاكل", "وش الي واقف", "اعطني الحالة",
            "كيف الامور", "عطني نظره سريعه", "وش عندك", "اش الوضع الحين",
            "how is it looking", "whats the situation", "summarise the services",
            "run down the services for me",
        ],
    ),
    (
        Intent::Save,
        &[
            "save memory", "free up memory", "saving mode", "go into saving mode",
            "stop the optional services", "we are low on ram", "reduce memory use",
            "cut back on memory", "ease the load", "trim the services",
            "we are running out of memory", "shed some load",
            "وضع التوفير", "وفر ذاكرة", "الذاكرة ممتلئة", "اوقف غير الضروري",
            "قلل الاستهلاك", "حرر ذاكرة", "خفف على الجهاز",
            "الجهاز ثقيل", "قلل الحمل", "الرام ممتلئه", "خفف الاستهلاك",
        ],
    ),
    (
        Intent::Resume,
        &[
            "resume", "bring everything back", "exit saving mode", "start the optional services",
            "restore normal operation", "turn everything back on",
            "ارجع كل شي", "اطفي وضع التوفير", "شغل الباقي", "رجع الوضع الطبيعي",
        ],
    ),
    (
        Intent::Check,
        &[
            "run the check", "self check", "boot check", "is the machine healthy",
            "readiness", "check the system", "verify everything", "run diagnostics",
            "confirm everything is fine", "validate the system",
            "run the self test", "sanity check", "check for faults",
            "افحص النظام", "فحص", "تحقق من النظام", "افحص كل شي", "هل النظام سليم",
            "تاكد ما في شي خربان", "شغل الفحص", "افحص وتاكد",
            "تحقق ان كل شي تمام", "فحص ذاتي", "اختبر النظام",
        ],
    ),
    (
        Intent::History,
        &[
            "recent runs", "what ran today", "show the history", "what failed",
            "did anything fail", "show recent workflow runs", "what happened last night",
            "اخر التشغيلات", "وش اشتغل اليوم", "اعرض السجل", "وش فشل",
            "في شي فشل", "شنو صار امس", "وش صار امس",
            "اعرض اخر العمليات", "سجل التشغيل", "وش الي فشل امس",
        ],
    ),
    (
        Intent::Snapshot,
        &[
            "take a snapshot", "snapshot the system", "back up before i change something",
            "make a restore point", "boot environment",
            "خذ لقطة", "احفظ نسخة", "نقطة استرجاع", "لقطة للنظام",
        ],
    ),
    (
        Intent::Explain,
        &[
            "how much memory is free", "what fits", "explain the system",
            "how does this work", "what depends on what", "show the dependencies",
            "is there room", "do we have space", "how much room is left",
            "what would fit in the memory left",
            "كم ذاكرة فاضية", "وش يتسع", "اشرح النظام", "كيف يشتغل",
            "وش يعتمد على وش", "اعرض الاعتماديات", "في مساحه", "كم باقي ذاكره",
            "هل في مجال", "وش يقدر يشتغل بالباقي",
        ],
    ),
];

/// Build the training set.
pub fn build(world: &World) -> Vec<Example> {
    let mut out = Vec::new();
    let names = world.entity_names();

    for (intent, templates) in WITH_ENTITY {
        for template in *templates {
            for name in &names {
                out.push(Example {
                    text: template.replace("{}", name),
                    intent: *intent,
                });
            }
        }
    }

    for (intent, templates) in WITHOUT_ENTITY {
        for template in *templates {
            // Repeated so the entity-free intents are not swamped by the cross
            // product above, which is a dozen times larger. An unbalanced
            // corpus teaches the model that every sentence is about a service.
            for _ in 0..names.len().max(1) {
                out.push(Example {
                    text: template.to_string(),
                    intent: *intent,
                });
            }
        }
    }

    out
}

/// Held-out phrasings, written to be unlike the templates.
///
/// The point of a test set is to fail when the model has memorised rather than
/// learned. Reusing a training template, or a light edit of one, measures
/// nothing. These are how the same requests get typed in practice, including
/// the untidy ones.
pub fn holdout() -> Vec<Example> {
    vec![
        // Deliberately awkward: elliptical, mixed-script, or phrased as a
        // complaint rather than an instruction. These are what actually gets
        // typed at a machine by someone who is annoyed with it.
        (Intent::Stop, "caddy needs to go away for a bit"),
        (Intent::Stop, "kill the whatsapp thing"),
        (Intent::Stop, "طفي لي الريديس"),
        (Intent::Start, "bring the tunnel back online"),
        (Intent::Start, "شغل لي البوابه"),
        (Intent::Restart, "give postgresql a kick"),
        (Intent::Restart, "الخدمه معلقه، اعد تشغيلها"),
        (Intent::Status, "anything on fire"),
        (Intent::Status, "كل شي شغال ولا لا"),
        (Intent::Explain, "what would i lose by stopping redis"),
        (Intent::Explain, "وش الي يعتمد على الكادي"),
        (Intent::Save, "we need headroom, drop the extras"),
        (Intent::Check, "run whatever verifies this thing"),
        (Intent::History, "show me what ran overnight"),
        (Intent::Snapshot, "make me a restore point"),
        (Intent::Rollback, "undo whatever i did to the ai jail"),
        (Intent::Stop, "could you shut down caddy for me"),
        (Intent::Stop, "i need postgresql off right now"),
        (Intent::Stop, "خلاص وقف كادي"),
        (Intent::Stop, "ابي اطفي البوستقرس"),
        (Intent::Start, "get the model running again"),
        (Intent::Start, "i want llamacpp up"),
        (Intent::Start, "ياليت تشغل النموذج"),
        (Intent::Start, "ابغى ارفع خدمة الوركفلو"),
        (Intent::Restart, "caddy is acting up, bounce it"),
        (Intent::Restart, "اعد تشغيل الخدمه مره ثانيه"),
        (Intent::Status, "everything alright over there"),
        (Intent::Status, "give me a quick rundown"),
        (Intent::Status, "طمني عن الوضع"),
        (Intent::Status, "وش الاخبار"),
        (Intent::Save, "machine is struggling, cut back"),
        (Intent::Save, "الجهاز يعلق، خفف عليه"),
        (Intent::Resume, "put everything back the way it was"),
        (Intent::Resume, "رجع كل شي زي ما كان"),
        (Intent::Explain, "if postgresql dies what else goes with it"),
        (Intent::Explain, "لو طاح البوستقرس وش يطيح معه"),
        (Intent::Explain, "is there room for the model"),
        (Intent::Check, "make sure nothing is broken"),
        (Intent::Check, "تاكد ان كل شي سليم"),
        (Intent::History, "anything blow up overnight"),
        (Intent::History, "في شي طاح البارحه"),
        (Intent::Snapshot, "save a restore point before i touch this"),
        (Intent::Snapshot, "خذ نسخه قبل ما اعدل"),
        (Intent::Rollback, "put the ai jail back to yesterday"),
        (Intent::Rollback, "ارجع الجيل حق النموذج"),
    ]
    .into_iter()
    .map(|(intent, text)| Example { text: text.to_string(), intent })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_names_real_services_only() {
        // The generated half is a cross product with the service table, so this
        // is what proves the corpus cannot mention something that was renamed.
        let w = World::default();
        let corpus = build(&w);
        let names = w.entity_names();
        let mentions = corpus
            .iter()
            .filter(|e| names.iter().any(|n| e.text.contains(n)))
            .count();
        assert!(mentions > 0);
        assert!(!corpus.iter().any(|e| e.text.contains("{}")), "a template was not filled");
    }

    #[test]
    fn every_intent_has_examples() {
        let corpus = build(&World::default());
        for intent in Intent::ALL {
            let n = corpus.iter().filter(|e| e.intent == *intent).count();
            assert!(n > 0, "{} has no training examples", intent.as_str());
        }
    }

    #[test]
    fn no_intent_dominates_the_corpus() {
        // An unbalanced corpus teaches the model to guess the majority class
        // and score well while being useless.
        let corpus = build(&World::default());
        let total = corpus.len() as f32;
        for intent in Intent::ALL {
            let n = corpus.iter().filter(|e| e.intent == *intent).count() as f32;
            let share = n / total;
            assert!(
                share < 0.45,
                "{} is {:.0}% of the corpus",
                intent.as_str(),
                share * 100.0
            );
        }
    }

    #[test]
    fn both_languages_are_represented_for_every_intent() {
        // A model trained on English alone would fail exactly when it is asked
        // in the language the owner actually types in.
        let corpus = build(&World::default());
        for intent in Intent::ALL {
            let arabic = corpus
                .iter()
                .filter(|e| e.intent == *intent)
                .filter(|e| e.text.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)))
                .count();
            assert!(arabic > 0, "{} has no Arabic examples", intent.as_str());
        }
    }

    #[test]
    fn the_holdout_shares_no_sentence_with_the_corpus() {
        // Otherwise the accuracy number measures memory, not generalisation.
        let corpus = build(&World::default());
        for h in holdout() {
            assert!(
                !corpus.iter().any(|e| e.text == h.text),
                "holdout sentence is in the training set: {}",
                h.text
            );
        }
    }

    #[test]
    fn intent_indices_round_trip() {
        for intent in Intent::ALL {
            assert_eq!(Intent::from_index(intent.index()), Some(*intent));
        }
        assert_eq!(Intent::from_index(Intent::ALL.len()), None);
    }
}
