//! ZFS snapshots and boot environments.
//!
//! The strongest thing FreeBSD offers this system, and the original plan never
//! mentioned it. Before anything risky: take a boot environment. If the change
//! goes wrong, reboot into yesterday and the whole system returns, not one
//! file.
//!
//! Two levels, because they answer different questions:
//!
//! - A **boot environment** (`bectl`) clones the root filesystem. It is what
//!   you want before an upgrade or a configuration change: the rollback is a
//!   reboot.
//! - A **dataset snapshot** (`zfs snapshot`) captures one service's data. It is
//!   what you want before a migration touches a database, and it rolls back
//!   without disturbing anything else.
//!
//! Names carry the reason and the time, so a list of snapshots reads as a
//! history rather than a pile of identifiers.

use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{tool} is not available: this is not a ZFS system")]
    Unavailable { tool: &'static str },
    #[error("{tool} failed: {stderr}")]
    Failed { tool: &'static str, stderr: String },
    #[error("'{0}' is not a usable name: use letters, digits, dash and underscore")]
    BadName(String),
}

/// Build a snapshot name from a reason and the current time.
///
/// `hajime-<reason>-<YYYYMMDD-HHMMSS>`. Sorting the list therefore sorts by
/// time, and the reason is visible without opening anything.
pub fn name_for(reason: &str, at: chrono::DateTime<chrono::Utc>) -> Result<String, SnapshotError> {
    let clean: String = reason
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let clean = clean.trim_matches('-').to_lowercase();
    if clean.is_empty() {
        return Err(SnapshotError::BadName(reason.to_string()));
    }
    Ok(format!("hajime-{clean}-{}", at.format("%Y%m%d-%H%M%S")))
}

fn run(tool: &'static str, args: &[&str]) -> Result<String, SnapshotError> {
    let output = Command::new(tool).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SnapshotError::Unavailable { tool }
        } else {
            SnapshotError::Failed { tool, stderr: e.to_string() }
        }
    })?;

    if !output.status.success() {
        return Err(SnapshotError::Failed {
            tool,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Clone the running root into a new boot environment.
///
/// Returns the name, which is what the operator types at the loader prompt to
/// go back.
pub fn create_boot_environment(reason: &str) -> Result<String, SnapshotError> {
    let name = name_for(reason, chrono::Utc::now())?;
    run("bectl", &["create", &name])?;
    Ok(name)
}

pub fn list_boot_environments() -> Result<Vec<String>, SnapshotError> {
    let out = run("bectl", &["list", "-H"])?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

/// Activate a boot environment. Takes effect on the next boot, deliberately:
/// switching the running root underneath a live system is not a thing to do
/// quietly.
pub fn activate_boot_environment(name: &str) -> Result<(), SnapshotError> {
    run("bectl", &["activate", name])?;
    Ok(())
}

/// Snapshot one dataset, for instance `zroot/vault/postgres`.
pub fn create_dataset_snapshot(dataset: &str, reason: &str) -> Result<String, SnapshotError> {
    let name = name_for(reason, chrono::Utc::now())?;
    let full = format!("{dataset}@{name}");
    run("zfs", &["snapshot", &full])?;
    Ok(full)
}

pub fn list_snapshots(dataset: Option<&str>) -> Result<Vec<String>, SnapshotError> {
    let mut args = vec!["list", "-H", "-t", "snapshot", "-o", "name"];
    if let Some(d) = dataset {
        args.push("-r");
        args.push(d);
    }
    let out = run("zfs", &args)?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Is this a ZFS system at all?
///
/// Checked rather than assumed: on a UFS install every call here would fail,
/// and the operator deserves to hear that once at startup instead of at the
/// moment they needed a rollback.
pub fn zfs_available() -> bool {
    run("zfs", &["version"]).is_ok() || run("zfs", &["list", "-H"]).is_ok()
}

pub fn boot_environments_available() -> bool {
    run("bectl", &["list", "-H"]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 8, 4, 9, 30, 15).unwrap()
    }

    #[test]
    fn a_name_carries_the_reason_and_the_time() {
        assert_eq!(
            name_for("before-migration", at()).unwrap(),
            "hajime-before-migration-20260804-093015"
        );
    }

    #[test]
    fn names_sort_chronologically() {
        // Listing snapshots should read as a history without extra work.
        let early = name_for("x", chrono::Utc.with_ymd_and_hms(2026, 8, 4, 1, 0, 0).unwrap()).unwrap();
        let late = name_for("x", chrono::Utc.with_ymd_and_hms(2026, 8, 4, 23, 0, 0).unwrap()).unwrap();
        assert!(early < late);
    }

    #[test]
    fn awkward_characters_are_replaced_not_rejected() {
        // A reason typed by a human should not fail; it should be tidied.
        assert_eq!(
            name_for("Before Upgrade: PostgreSQL 17!", at()).unwrap(),
            "hajime-before-upgrade--postgresql-17-20260804-093015"
        );
    }

    #[test]
    fn arabic_reasons_do_not_produce_an_empty_name() {
        // Non-ASCII collapses to dashes, which would leave nothing behind.
        // Better to refuse than to create `hajime--20260804-093015`.
        assert!(matches!(name_for("قبل الترقية", at()), Err(SnapshotError::BadName(_))));
    }

    #[test]
    fn an_empty_reason_is_refused() {
        assert!(name_for("", at()).is_err());
        assert!(name_for("---", at()).is_err());
    }

    #[test]
    fn a_missing_tool_is_reported_as_unavailable_not_as_failure() {
        // On Windows and on a UFS FreeBSD install neither tool exists. The
        // distinction matters: "not a ZFS system" is a configuration fact,
        // "the command failed" is a bug to chase.
        match run("definitely-not-a-real-tool-xyz", &["--help"]) {
            Err(SnapshotError::Unavailable { .. }) => {}
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn availability_checks_do_not_panic_off_freebsd() {
        // These run on the development machine too; they must answer, not
        // explode.
        let _ = zfs_available();
        let _ = boot_environments_available();
    }
}
