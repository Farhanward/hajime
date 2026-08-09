//! Jails, and the per-jail rollback they exist for.
//!
//! The point is not isolation for its own sake. It is that each jail sits on
//! its own ZFS dataset, so one service can be snapshotted, updated and rolled
//! back without touching the other two. A boot environment rolls the whole
//! machine back; this rolls back the model, or the web stack, alone.
//!
//! Three jails rather than one per service. A jail costs a dataset and a
//! routing entry, but splitting services that share a socket into separate
//! jails costs a network hop and a debugging session. The split follows how
//! exposed each group is:
//!
//! - `web` faces the internet, runs other people's code (PHP), and is the one
//!   that gets broken into. Nothing else lives with it.
//! - `data` holds the databases. Reachable from the other jails, never from
//!   outside.
//! - `ai` runs the model and llama.cpp. Jailed because a model that can be
//!   talked into running a tool should not be talking to a host shell.
//!
//! What stays on the host is what needs the host: the workflow engine reaches
//! everything by design, cloudflared owns the tunnel, and hajimectl manages the
//! jails and cannot live inside one.

use crate::snapshot::{self, SnapshotError};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jail {
    pub name: &'static str,
    pub description: &'static str,
    /// Where the jail root sits under the pool. The pool itself is read from
    /// the machine rather than written here: FreeBSD's installer calls it
    /// `zroot` by default, so hardcoding that is right on a default install and
    /// wrong on every other one, silently.
    pub dataset_suffix: &'static str,
    /// Address on the internal jail network.
    pub address: &'static str,
    /// Services expected to run inside.
    pub holds: &'static str,
}

/// Every jail this system defines.
///
/// One table, as with services: a jail that exists in the config but not here
/// is a jail nothing manages, and the two drifting apart is how a rollback
/// silently misses a dataset.
pub static JAILS: &[Jail] = &[
    Jail {
        name: "web",
        description: "public sites and the PHP store",
        dataset_suffix: "jails/web",
        address: "10.99.0.10",
        holds: "caddy, litecart",
    },
    Jail {
        name: "data",
        description: "databases, reachable only from the other jails",
        dataset_suffix: "jails/data",
        address: "10.99.0.20",
        holds: "postgresql, mariadb, redis",
    },
    Jail {
        name: "ai",
        description: "the model and its inference engine",
        dataset_suffix: "jails/ai",
        address: "10.99.0.30",
        holds: "llamacpp, hajime_ai",
    },
];

/// The pool the jails live on.
///
/// The first pool `zpool list` reports, which on a machine with one pool is the
/// only answer there is. `HAJIME_POOL` overrides it for a machine with several,
/// where "the first one" is a guess rather than an answer.
pub fn pool() -> String {
    if let Ok(name) = std::env::var("HAJIME_POOL") {
        if !name.trim().is_empty() {
            return name.trim().to_string();
        }
    }
    Command::new("zpool")
        .args(["list", "-H", "-o", "name"])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .and_then(|s| s.lines().next().map(str::trim).map(str::to_string))
        })
        .filter(|s| !s.is_empty())
        // Only as a last resort, and only so the messages read sensibly on a
        // machine with no pool at all, where every operation refuses anyway.
        .unwrap_or_else(|| "zroot".to_string())
}

impl Jail {
    /// The full dataset name on this machine.
    pub fn dataset(&self) -> String {
        format!("{}/{}", pool(), self.dataset_suffix)
    }
}

pub fn find(name: &str) -> Option<&'static Jail> {
    JAILS.iter().find(|j| j.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Stopped,
    /// Defined here but absent from the running config: the jail was never
    /// created, or its dataset is gone.
    Missing,
}

impl State {
    pub fn label(&self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Missing => "not created",
        }
    }
}

/// Ask `jls` which jails are running.
///
/// A missing `jls` means this is not FreeBSD, which is reported rather than
/// treated as "nothing is running": those look the same to a caller and mean
/// opposite things.
pub fn running() -> Result<Vec<String>, JailError> {
    let out = Command::new("jls")
        .args(["-h", "name"])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                JailError::Unavailable
            } else {
                JailError::Failed(e.to_string())
            }
        })?;
    if !out.status.success() {
        return Err(JailError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip(1) // the header `name`
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

pub fn state_of(jail: &Jail, running_now: &[String]) -> State {
    if running_now.iter().any(|n| n == jail.name) {
        return State::Running;
    }
    if dataset_exists(&jail.dataset()) {
        State::Stopped
    } else {
        State::Missing
    }
}

fn dataset_exists(dataset: &str) -> bool {
    Command::new("zfs")
        .args(["list", "-H", "-o", "name", dataset])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Snapshot one jail's dataset before something risky happens to it.
///
/// Refuses when the dataset is not there. Reporting success for a snapshot that
/// was never taken is the failure this whole module exists to prevent.
pub fn snapshot(jail: &Jail, reason: &str) -> Result<String, JailError> {
    let dataset = jail.dataset();
    if !dataset_exists(&dataset) {
        return Err(JailError::NoDataset { jail: jail.name, dataset });
    }
    snapshot::create_dataset_snapshot(&dataset, reason).map_err(JailError::Snapshot)
}

pub fn snapshots(jail: &Jail) -> Result<Vec<String>, JailError> {
    snapshot::list_snapshots(Some(&jail.dataset())).map_err(JailError::Snapshot)
}

/// Roll a jail back to a snapshot.
///
/// `zfs rollback -r` destroys every snapshot taken after the target, which is
/// not undoable. The jail is required to be stopped first: rolling back a
/// mounted, running root gives the processes inside a filesystem that changed
/// under them, and the damage shows up later as corruption rather than as an
/// error here.
pub fn rollback(jail: &Jail, snapshot_name: &str, running_now: &[String]) -> Result<(), JailError> {
    if running_now.iter().any(|n| n == jail.name) {
        return Err(JailError::StillRunning(jail.name));
    }
    let dataset = jail.dataset();
    let full = if snapshot_name.contains('@') {
        snapshot_name.to_string()
    } else {
        format!("{dataset}@{snapshot_name}")
    };
    if !full.starts_with(&format!("{dataset}@")) {
        return Err(JailError::WrongDataset {
            jail: jail.name,
            given: full,
        });
    }
    let out = Command::new("zfs")
        .args(["rollback", "-r", &full])
        .output()
        .map_err(|e| JailError::Failed(e.to_string()))?;
    if !out.status.success() {
        return Err(JailError::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum JailError {
    #[error("jls is not available: this is not a FreeBSD host")]
    Unavailable,
    #[error("{0}")]
    Failed(String),
    #[error("jail '{jail}' has no dataset at {dataset}; create it before snapshotting")]
    NoDataset { jail: &'static str, dataset: String },
    #[error("jail '{0}' is running; stop it before rolling back")]
    StillRunning(&'static str),
    #[error("'{given}' does not belong to jail '{jail}'")]
    WrongDataset { jail: &'static str, given: String },
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_jail_has_its_own_dataset_and_address() {
        // Two jails sharing a dataset would make a rollback of one silently
        // roll back the other, which is the opposite of why they are split.
        for a in JAILS {
            for b in JAILS {
                if a.name == b.name {
                    continue;
                }
                assert_ne!(
                    a.dataset(),
                    b.dataset(),
                    "{} and {} share a dataset",
                    a.name,
                    b.name
                );
                assert_ne!(a.address, b.address, "{} and {} share an address", a.name, b.name);
            }
        }
    }

    #[test]
    fn jail_addresses_are_on_the_private_network() {
        // A jail that picked up a routable address would be exposed directly
        // rather than through Caddy.
        for j in JAILS {
            assert!(j.address.starts_with("10."), "{} is not on 10/8: {}", j.name, j.address);
        }
    }

    #[test]
    fn the_dataset_follows_the_machines_pool_rather_than_a_fixed_name() {
        // The bug this replaces: the datasets were the literal string
        // "zroot/jails/web". FreeBSD's installer calls the pool zroot by
        // default, so that is right on a default install and silently wrong on
        // any other. It surfaced on a guest whose pool was called hajimepool,
        // where every jail reported "not created" and no rollback could find
        // anything.
        std::env::set_var("HAJIME_POOL", "tank");
        let web = find("web").unwrap();
        assert_eq!(web.dataset(), "tank/jails/web");

        std::env::set_var("HAJIME_POOL", "hajimepool");
        assert_eq!(web.dataset(), "hajimepool/jails/web");
        std::env::remove_var("HAJIME_POOL");
    }

    #[test]
    fn a_blank_pool_override_is_ignored_rather_than_producing_a_leading_slash() {
        std::env::set_var("HAJIME_POOL", "   ");
        let web = find("web").unwrap();
        assert!(!web.dataset().starts_with('/'), "{}", web.dataset());
        std::env::remove_var("HAJIME_POOL");
    }

    #[test]
    fn find_matches_by_name_only() {
        assert_eq!(find("web").unwrap().name, "web");
        assert!(find("Web").is_none(), "lookup must not be case-insensitive");
        assert!(find("nope").is_none());
    }

    #[test]
    fn a_rollback_target_from_another_jail_is_refused() {
        // Passing the fully-qualified name of the wrong jail's snapshot is an
        // easy slip when both are on screen, and it destroys the wrong data.
        let web = find("web").unwrap();
        let other = find("ai").unwrap().dataset();
        let err = rollback(web, &format!("{other}@hajime-x-20260804-120000"), &[]).unwrap_err();
        assert!(matches!(err, JailError::WrongDataset { .. }), "{err:?}");
    }

    #[test]
    fn a_running_jail_is_not_rolled_back() {
        let web = find("web").unwrap();
        let running = vec!["web".to_string()];
        let err = rollback(web, "hajime-x-20260804-120000", &running).unwrap_err();
        assert!(matches!(err, JailError::StillRunning("web")), "{err:?}");
    }

    #[test]
    fn state_reports_running_before_looking_at_the_disk() {
        let web = find("web").unwrap();
        assert_eq!(state_of(web, &["web".to_string()]), State::Running);
    }

    #[test]
    fn state_labels_read_as_english() {
        assert_eq!(State::Missing.label(), "not created");
        assert_eq!(State::Running.label(), "running");
    }
}
