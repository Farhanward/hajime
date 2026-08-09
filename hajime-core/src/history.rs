//! Execution history.
//!
//! n8n keeps every run so an operator can answer "did the 03:20 job fire, and
//! what did it do". A replacement without that is a downgrade, and during a
//! migration it is the only way to compare the two engines over time.
//!
//! Records are appended as JSON Lines. One line per run, flushed immediately,
//! so a crash costs at most the run in flight and a partial trailing line is
//! skipped on read rather than poisoning the file. Item payloads are not
//! stored: they routinely carry credentials and page bodies, and this file is
//! not the place for either.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub workflow_id: String,
    pub workflow: String,
    pub trigger: String,
    pub success: bool,
    pub started_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub nodes_run: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Record {
    /// A successful run.
    ///
    /// Deliberately built from plain fields rather than from a caller's result
    /// type: this crate must not learn what a workflow, a model or a message
    /// is. Each pillar converts its own outcome into a record.
    pub fn success(
        subject_id: impl Into<String>,
        subject: impl Into<String>,
        trigger: impl Into<String>,
        started_at: DateTime<Utc>,
        duration_ms: u64,
        steps: usize,
    ) -> Self {
        Self {
            workflow_id: subject_id.into(),
            workflow: subject.into(),
            trigger: trigger.into(),
            success: true,
            started_at,
            duration_ms,
            nodes_run: steps,
            failed_node: None,
            error: None,
        }
    }

    /// A failed run, naming the step that failed and why.
    #[allow(clippy::too_many_arguments)]
    pub fn failure(
        subject_id: impl Into<String>,
        subject: impl Into<String>,
        trigger: impl Into<String>,
        started_at: DateTime<Utc>,
        duration_ms: u64,
        steps: usize,
        failed_step: Option<String>,
        error: Option<String>,
    ) -> Self {
        Self {
            workflow_id: subject_id.into(),
            workflow: subject.into(),
            trigger: trigger.into(),
            success: false,
            started_at,
            duration_ms,
            nodes_run: steps,
            failed_node: failed_step,
            error,
        }
    }
}

pub struct History {
    path: Option<PathBuf>,
    /// Serialises appends so two concurrent runs cannot interleave a line.
    lock: Mutex<()>,
}

impl History {
    /// A history that discards everything. Used when no path is configured.
    pub fn disabled() -> Self {
        Self { path: None, lock: Mutex::new(()) }
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: Some(path.into()), lock: Mutex::new(()) }
    }

    pub fn from_env() -> Self {
        match std::env::var("HAJIME_HISTORY") {
            Ok(p) if !p.trim().is_empty() => Self::at(p),
            _ => Self::disabled(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Append one record. A write failure is reported but never aborts a run:
    /// losing the log of a successful job is better than failing the job.
    pub fn append(&self, record: &Record) {
        let Some(path) = &self.path else { return };
        let line = match serde_json::to_string(record) {
            Ok(line) => line,
            Err(e) => {
                tracing::error!(error = %e, "could not encode history record");
                return;
            }
        };

        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
        match opened {
            Ok(mut file) => {
                if let Err(e) = writeln!(file, "{line}").and_then(|_| file.flush()) {
                    tracing::error!(error = %e, "could not write history");
                }
            }
            Err(e) => tracing::error!(error = %e, path = %path.display(), "could not open history"),
        }
    }

    /// The most recent `limit` records, newest first.
    ///
    /// A malformed line is skipped rather than failing the read: a truncated
    /// tail from an unclean shutdown must not hide the rest of the history.
    pub fn recent(&self, limit: usize) -> Vec<Record> {
        let Some(path) = &self.path else { return Vec::new() };
        let mut all = read_all(path);
        all.reverse();
        all.truncate(limit);
        all
    }

    pub fn stats(&self) -> (usize, usize) {
        let Some(path) = &self.path else { return (0, 0) };
        let all = read_all(path);
        let failed = all.iter().filter(|r| !r.success).count();
        (all.len(), failed)
    }
}

fn read_all(path: &Path) -> Vec<Record> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_record(name: &str) -> Record {
        Record::success("wf", name, "manual", Utc::now(), 7, 2)
    }

    fn bad_record() -> Record {
        Record::failure(
            "wf",
            "Health",
            "Every Hour",
            Utc::now(),
            7,
            2,
            Some("Fetch".into()),
            Some("connection refused".into()),
        )
    }

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("hajime_hist_{name}.jsonl"));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_disabled_history_records_nothing_and_never_panics() {
        let h = History::disabled();
        assert!(!h.is_enabled());
        h.append(&ok_record("Health"));
        assert!(h.recent(10).is_empty());
        assert_eq!(h.stats(), (0, 0));
    }

    #[test]
    fn records_round_trip_newest_first() {
        let path = temp("round");
        let h = History::at(&path);
        for i in 0..3 {
            h.append(&ok_record(&format!("run-{i}")));
        }
        let recent = h.recent(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].workflow, "run-2", "newest should come first");
        assert_eq!(recent[2].workflow, "run-0");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_failure_captures_the_step_and_reason() {
        let path = temp("fail");
        let h = History::at(&path);
        h.append(&bad_record());

        let rec = &h.recent(1)[0];
        assert!(!rec.success);
        assert_eq!(rec.failed_node.as_deref(), Some("Fetch"));
        assert_eq!(rec.error.as_deref(), Some("connection refused"));
        assert_eq!(rec.trigger, "Every Hour");
        assert_eq!(rec.duration_ms, 7);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_truncated_trailing_line_does_not_hide_the_rest() {
        let path = temp("torn");
        let h = History::at(&path);
        h.append(&ok_record("Health"));
        // Simulate a crash mid-write.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "{{\"workflow_id\":\"half").unwrap();
        drop(f);

        assert_eq!(h.recent(10).len(), 1, "the intact record should survive");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stats_count_totals_and_failures() {
        let path = temp("stats");
        let h = History::at(&path);
        h.append(&ok_record("a"));
        h.append(&bad_record());
        h.append(&ok_record("b"));
        assert_eq!(h.stats(), (3, 1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn payloads_are_never_written() {
        let path = temp("nopayload");
        let h = History::at(&path);
        h.append(&ok_record("Health"));
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("outputs"), "history must not carry item data");
        assert!(!raw.contains("items_out"), "history must not carry item data");
        let _ = std::fs::remove_file(&path);
    }
}
