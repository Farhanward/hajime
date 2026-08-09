//! Cron-driven runs.
//!
//! Timezone is explicit and required. n8n evaluates a schedule in the
//! instance's timezone, and a host that ignores this gets it wrong quietly:
//! reading `0 * * * *` as UTC when the instance runs `Asia/Riyadh` (UTC+3)
//! fires every job three hours off, which is the kind of fault that looks
//! like the scheduler working.

use crate::engine::Engine;
use crate::engine::record_of;
use crate::history::History;
use crate::store::Store;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// One resolved schedule and when it next fires.
#[derive(Debug, Clone)]
pub struct Pending {
    pub workflow_id: String,
    pub node: String,
    pub expression: String,
    pub next: DateTime<Utc>,
}

/// What to do about schedules that came due while the service was not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Catchup {
    /// Report each missed occurrence and run none of them.
    ///
    /// The default, and what n8n does. A daily job missed for a week should not
    /// fire seven times the moment the machine comes back.
    Report,
    /// Report them, then run each schedule once.
    ///
    /// For a job whose point is that it happened at all, not that it happened
    /// on the hour. Still one run per schedule, never one per occurrence.
    RunOnce,
}

impl Catchup {
    pub fn from_env() -> Self {
        match std::env::var("HAJIME_CATCHUP").as_deref() {
            Ok("1") | Ok("once") | Ok("yes") => Catchup::RunOnce,
            _ => Catchup::Report,
        }
    }
}

pub struct Scheduler {
    store: Arc<Store>,
    engine: Arc<Engine>,
    history: Arc<History>,
    tz: Tz,
    /// Where the last tick is written, so a restart knows what it slept
    /// through. Without it the gap is invisible: the loop starts from `now`
    /// and everything that came due while the process was down is dropped with
    /// nothing recorded anywhere.
    state_path: Option<std::path::PathBuf>,
    catchup: Catchup,
}

impl Scheduler {
    pub fn new(
        store: Arc<Store>,
        engine: Arc<Engine>,
        history: Arc<History>,
        tz: Tz,
    ) -> Self {
        Self {
            store,
            engine,
            history,
            tz,
            state_path: None,
            catchup: Catchup::Report,
        }
    }

    /// Remember the last tick across restarts.
    pub fn with_state_file(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.state_path = Some(path.into());
        self
    }

    pub fn with_catchup(mut self, catchup: Catchup) -> Self {
        self.catchup = catchup;
        self
    }

    /// Read the tick recorded by the previous run of this process.
    ///
    /// A missing or unreadable file yields `None`, which is treated as a first
    /// start rather than as a gap: inventing a gap would report every schedule
    /// as missed on a fresh install.
    fn load_last_tick(&self) -> Option<DateTime<Utc>> {
        let path = self.state_path.as_ref()?;
        let raw = std::fs::read_to_string(path).ok()?;
        DateTime::parse_from_rfc3339(raw.trim())
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    fn save_tick(&self, at: DateTime<Utc>) {
        let Some(path) = &self.state_path else { return };
        // Write and rename, so a crash mid-write leaves the old tick rather
        // than a truncated file that parses as "no previous tick" and hides a
        // gap on the next start.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, at.to_rfc3339()).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, path) {
                tracing::warn!(error = %e, "could not record the scheduler tick");
            }
        }
    }

    /// Occurrences of each schedule between two times, capped.
    ///
    /// The cap matters: a per-minute schedule and a week of downtime is ten
    /// thousand occurrences, and the caller only ever needs to know that the
    /// job was missed and roughly how often.
    pub fn missed(
        &self,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Vec<(Pending, usize)> {
        const CAP: usize = 500;
        let mut out = Vec::new();
        for s in self.store.schedules() {
            let Ok(cron) = Cron::new(&s.expression).parse() else {
                continue;
            };
            let mut cursor = since.with_timezone(&self.tz);
            let mut count = 0usize;
            let mut first: Option<DateTime<Utc>> = None;
            while count < CAP {
                let Ok(next) = cron.find_next_occurrence(&cursor, false) else {
                    break;
                };
                let next_utc = next.with_timezone(&Utc);
                if next_utc > now {
                    break;
                }
                if first.is_none() {
                    first = Some(next_utc);
                }
                cursor = next;
                count += 1;
            }
            if let Some(at) = first {
                out.push((
                    Pending {
                        workflow_id: s.workflow_id,
                        node: s.node,
                        expression: s.expression,
                        next: at,
                    },
                    count,
                ));
            }
        }
        out
    }

    /// The next fire time for each active schedule, soonest first.
    ///
    /// `now` is a parameter so this can be tested without waiting for a clock.
    pub fn upcoming(&self, now: DateTime<Utc>) -> Vec<Pending> {
        let local = now.with_timezone(&self.tz);
        let mut out: Vec<Pending> = self
            .store
            .schedules()
            .into_iter()
            .filter_map(|s| {
                let cron = Cron::new(&s.expression).parse().ok()?;
                let next = cron.find_next_occurrence(&local, false).ok()?;
                Some(Pending {
                    workflow_id: s.workflow_id,
                    node: s.node,
                    expression: s.expression,
                    next: next.with_timezone(&Utc),
                })
            })
            .collect();
        out.sort_by_key(|p| p.next);
        out
    }

    /// Schedules due at or before `now`.
    pub fn due(&self, previous_tick: DateTime<Utc>, now: DateTime<Utc>) -> Vec<Pending> {
        self.store
            .schedules()
            .into_iter()
            .filter_map(|s| {
                let cron = Cron::new(&s.expression).parse().ok()?;
                let from = previous_tick.with_timezone(&self.tz);
                let next = cron.find_next_occurrence(&from, false).ok()?;
                let next_utc = next.with_timezone(&Utc);
                (next_utc <= now).then_some(Pending {
                    workflow_id: s.workflow_id,
                    node: s.node,
                    expression: s.expression,
                    next: next_utc,
                })
            })
            .collect()
    }

    /// Run the loop until the process ends.
    ///
    /// Ticks once a minute on the minute boundary. Cron's finest granularity
    /// here is the minute, so a faster tick would only risk firing twice.
    pub async fn run_forever(self) {
        let boot = Utc::now();

        // What did the machine sleep through? Answering this at startup is the
        // whole reason the tick is persisted. Previously the loop began at
        // `now`, so a service that was down overnight came back believing
        // nothing had been due, and the jobs it skipped left no trace in the
        // history, the logs or the console.
        let mut previous = match self.load_last_tick() {
            Some(last) if last < boot => {
                let gap_min = (boot - last).num_minutes().max(0);
                let missed = self.missed(last, boot);
                if missed.is_empty() {
                    tracing::info!(
                        gap_minutes = gap_min,
                        "the scheduler was down but nothing came due"
                    );
                } else {
                    tracing::warn!(
                        gap_minutes = gap_min,
                        schedules = missed.len(),
                        "schedules came due while the scheduler was not running"
                    );
                    for (pending, count) in &missed {
                        let name = self
                            .store
                            .get(&pending.workflow_id)
                            .map(|w| w.name)
                            .unwrap_or_else(|| pending.workflow_id.clone());
                        tracing::warn!(
                            workflow = %name,
                            node = %pending.node,
                            occurrences = count,
                            first_missed = %pending.next,
                            "missed"
                        );
                        // Recorded as a failed run so it appears on the console
                        // and in the history. A missed job that is only in the
                        // log is a missed job nobody sees.
                        self.history.append(&crate::history::Record::failure(
                            pending.workflow_id.clone(),
                            name,
                            "schedule",
                            pending.next,
                            0,
                            0,
                            Some(pending.node.clone()),
                            Some(format!(
                                "missed: {count} occurrence(s) of '{}' came due \
                                 while the scheduler was down for {gap_min} minute(s)",
                                pending.expression
                            )),
                        ));
                    }

                    if self.catchup == Catchup::RunOnce {
                        tracing::warn!(
                            "HAJIME_CATCHUP is set: running each missed schedule once"
                        );
                        for (pending, _) in &missed {
                            if let Some(workflow) = self.store.get(&pending.workflow_id) {
                                let engine = Arc::clone(&self.engine);
                                let history = Arc::clone(&self.history);
                                let id = pending.workflow_id.clone();
                                let node = pending.node.clone();
                                let started = Utc::now();
                                tokio::spawn(async move {
                                    let outcome =
                                        engine.run(&workflow, Some(&node), vec![]).await;
                                    if let Ok(result) = &outcome {
                                        history.append(&record_of(
                                            &id, &node, started, result,
                                        ));
                                    }
                                });
                            }
                        }
                    }
                }
                boot
            }
            _ => boot,
        };
        self.save_tick(previous);

        loop {
            tokio::time::sleep(Self::until_next_minute()).await;
            let now = Utc::now();
            for pending in self.due(previous, now) {
                let Some(workflow) = self.store.get(&pending.workflow_id) else {
                    continue;
                };
                let engine = Arc::clone(&self.engine);
                let history = Arc::clone(&self.history);
                let workflow_id = pending.workflow_id.clone();
                let node = pending.node.clone();
                let started_at = Utc::now();
                // Each run is detached so a slow workflow cannot delay the
                // next tick or block its neighbours.
                tokio::spawn(async move {
                    let outcome = engine.run(&workflow, Some(&node), vec![]).await;
                    if let Ok(result) = &outcome {
                        history.append(&record_of(&workflow_id, &node, started_at, result,
                        ));
                    }
                    match outcome {
                        Ok(result) if result.success => {
                            tracing::info!(
                                workflow = %workflow.name,
                                ms = result.duration_us / 1000,
                                "scheduled run finished"
                            );
                        }
                        Ok(result) => {
                            let failed = result.runs.iter().find(|r| r.error.is_some());
                            tracing::error!(
                                workflow = %workflow.name,
                                node = failed.map(|r| r.node.as_str()).unwrap_or("?"),
                                error = failed.and_then(|r| r.error.as_deref()).unwrap_or("?"),
                                "scheduled run failed"
                            );
                        }
                        Err(e) => tracing::error!(
                            workflow = %workflow.name,
                            error = %e,
                            "scheduled run could not start"
                        ),
                    }
                });
            }
            previous = now;
            // After the tick, not before: a crash between the two makes the
            // next start think it missed a minute, which is reported. The other
            // order would make it think it ran a minute it never did.
            self.save_tick(now);
        }
    }

    fn until_next_minute() -> Duration {
        let now = Utc::now();
        let secs = 60 - (now.timestamp() % 60) as u64;
        Duration::from_secs(secs.max(1))
    }
}

/// Read a timezone name, falling back to the host the workflows came from.
pub fn timezone(name: Option<&str>) -> Tz {
    name.and_then(|n| Tz::from_str(n).ok())
        .unwrap_or(chrono_tz::Asia::Riyadh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::Registry;
    use chrono::TimeZone;

    fn store_with(expression: &str) -> Arc<Store> {
        let export = format!(
            r#"[{{"id":"x","name":"S","active":true,"nodes":[
                 {{"name":"Tick","type":"n8n-nodes-base.scheduleTrigger",
                   "parameters":{{"rule":{{"interval":[
                     {{"field":"cronExpression","expression":"{expression}"}}]}}}}}}
               ],"connections":{{}}}}]"#
        );
        Arc::new(Store::from_export(&export).unwrap())
    }

    /// A scratch path that does not collide between tests.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hajime-sched-{tag}-{}.tick", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_gap_reports_every_schedule_that_came_due() {
        // Hourly job, six hours of downtime. All six must be visible; the
        // version this replaces reported none of them anywhere.
        let s = scheduler("0 * * * *", chrono_tz::UTC);
        let from = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 8, 4, 6, 30, 0).unwrap();
        let missed = s.missed(from, to);
        assert_eq!(missed.len(), 1, "one schedule");
        assert_eq!(missed[0].1, 6, "six occurrences between 00:00 and 06:30");
        assert_eq!(
            missed[0].0.next,
            Utc.with_ymd_and_hms(2026, 8, 4, 1, 0, 0).unwrap(),
            "the first missed one is 01:00"
        );
    }

    #[test]
    fn no_gap_reports_nothing() {
        let s = scheduler("0 * * * *", chrono_tz::UTC);
        let from = Utc.with_ymd_and_hms(2026, 8, 4, 0, 1, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 8, 4, 0, 59, 0).unwrap();
        assert!(s.missed(from, to).is_empty(), "nothing was due in that window");
    }

    #[test]
    fn a_long_gap_on_a_frequent_schedule_is_capped() {
        // Every minute for a year would be half a million entries. The count is
        // capped because the operator needs to know it was missed, not to read
        // a list of every minute.
        let s = scheduler("* * * * *", chrono_tz::UTC);
        let from = Utc.with_ymd_and_hms(2025, 8, 4, 0, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 8, 4, 0, 0, 0).unwrap();
        let missed = s.missed(from, to);
        assert_eq!(missed[0].1, 500, "capped rather than unbounded");
    }

    #[test]
    fn the_tick_survives_a_restart() {
        let path = scratch("roundtrip");
        let s = scheduler("0 * * * *", chrono_tz::UTC).with_state_file(&path);
        assert!(s.load_last_tick().is_none(), "no file yet means a first start");

        let at = Utc.with_ymd_and_hms(2026, 8, 4, 3, 0, 0).unwrap();
        s.save_tick(at);
        assert_eq!(s.load_last_tick(), Some(at));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_tick_file_reads_as_a_first_start_not_as_a_huge_gap() {
        // Treating garbage as "the epoch" would report every schedule as
        // missed thousands of times and bury the real history.
        let path = scratch("corrupt");
        std::fs::write(&path, "not a timestamp").unwrap();
        let s = scheduler("0 * * * *", chrono_tz::UTC).with_state_file(&path);
        assert!(s.load_last_tick().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn catchup_is_off_unless_asked_for() {
        // Running everything missed by default would fire a week of daily jobs
        // at once the moment the machine came back.
        assert_eq!(Catchup::Report, Scheduler::new(
            store_with("0 * * * *"),
            Arc::new(Engine::new(Registry::empty())),
            Arc::new(History::disabled()),
            chrono_tz::UTC,
        ).catchup);
    }

    fn scheduler(expression: &str, tz: Tz) -> Scheduler {
        Scheduler::new(
            store_with(expression),
            Arc::new(Engine::new(Registry::empty())),
            Arc::new(History::disabled()),
            tz,
        )
    }

    #[test]
    fn hourly_schedule_resolves_to_the_next_hour() {
        let s = scheduler("0 * * * *", chrono_tz::Asia::Riyadh);
        // 09:30 UTC == 12:30 Riyadh, so the next top of the hour is 13:00
        // Riyadh, which is 10:00 UTC.
        let now = Utc.with_ymd_and_hms(2026, 8, 3, 9, 30, 0).unwrap();
        let next = s.upcoming(now);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].next, Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 0).unwrap());
    }

    #[test]
    fn timezone_shifts_a_daily_schedule() {
        // "20 3 * * *" is 03:20 local. In Riyadh that is 00:20 UTC.
        let riyadh = scheduler("20 3 * * *", chrono_tz::Asia::Riyadh);
        let now = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
        assert_eq!(
            riyadh.upcoming(now)[0].next,
            Utc.with_ymd_and_hms(2026, 8, 3, 0, 20, 0).unwrap()
        );

        // The same expression read as UTC fires three hours later. This is the
        // difference the timezone parameter exists to prevent.
        let utc = scheduler("20 3 * * *", chrono_tz::UTC);
        assert_eq!(
            utc.upcoming(now)[0].next,
            Utc.with_ymd_and_hms(2026, 8, 3, 3, 20, 0).unwrap()
        );
    }

    #[test]
    fn a_schedule_is_due_once_its_moment_has_passed() {
        let s = scheduler("0 * * * *", chrono_tz::Asia::Riyadh);
        let before = Utc.with_ymd_and_hms(2026, 8, 3, 9, 59, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 8, 3, 10, 0, 30).unwrap();

        assert_eq!(s.due(before, after).len(), 1, "should fire at the hour");
        // Nothing new between two points inside the same hour.
        let mid = Utc.with_ymd_and_hms(2026, 8, 3, 10, 30, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 8, 3, 10, 45, 0).unwrap();
        assert!(s.due(mid, later).is_empty(), "should not fire twice");
    }

    #[test]
    fn every_schedule_in_the_export_resolves() {
        let now = Utc.with_ymd_and_hms(2026, 8, 3, 0, 0, 0).unwrap();
        for expr in ["30 9 * * 1", "20 3 * * *", "0 * * * *", "12 0 * * *"] {
            let s = scheduler(expr, chrono_tz::Asia::Riyadh);
            assert_eq!(s.upcoming(now).len(), 1, "{expr} produced no next time");
        }
    }

    #[test]
    fn timezone_lookup_falls_back_to_the_host_zone() {
        assert_eq!(timezone(Some("Europe/Berlin")), chrono_tz::Europe::Berlin);
        assert_eq!(timezone(Some("not/a/zone")), chrono_tz::Asia::Riyadh);
        assert_eq!(timezone(None), chrono_tz::Asia::Riyadh);
    }
}
