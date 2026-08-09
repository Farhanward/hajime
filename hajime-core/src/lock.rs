//! At-most-once: one process per job, enforced by the kernel.
//!
//! The workflow engine fires schedules and posts to live systems. Two copies
//! running at once means every scheduled job happens twice, and the second one
//! is invisible: both processes log success, both write history, and the only
//! evidence is a duplicated post or a doubled charge somewhere downstream.
//!
//! Binding the HTTP port already stops a second copy on the same port, but not
//! one started with a different `HAJIME_BIND`, and the scheduler begins ticking
//! before the listener binds. So the lock is explicit.
//!
//! `flock` rather than a pid file. A pid file has to answer "is that process
//! still alive", which means guessing across a pid that may have been reused,
//! and it survives a crash as a stale lock that blocks the restart. An advisory
//! lock is released by the kernel when the process dies, however it dies.

use fs4::fs_std::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another process already holds {path}: {hint}")]
    Held { path: String, hint: String },
    #[error("could not open the lock file {path}: {source}")]
    Unopenable {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// A held lock. Dropping it, or the process ending, releases it.
///
/// The file is kept open on purpose: the lock lives on the descriptor, so
/// closing the file would release it while the program carried on believing it
/// still held it.
#[derive(Debug)]
pub struct Lock {
    _file: File,
    path: PathBuf,
}

impl Lock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Take an exclusive lock, or report who holds it.
///
/// Never blocks. A service that waits for a lock at startup looks hung, and rc
/// will time it out anyway.
pub fn acquire(path: impl Into<PathBuf>) -> Result<Lock, LockError> {
    let path = path.into();

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| LockError::Unopenable {
            path: path.display().to_string(),
            source,
        })?;

    match FileExt::try_lock_exclusive(&file) {
        Ok(true) => {
            // Written for a human reading the file, not for the lock itself:
            // the kernel holds the lock, this is only a note about who.
            use std::io::{Seek, SeekFrom, Write};
            let mut f = &file;
            let _ = f.set_len(0);
            let _ = f.seek(SeekFrom::Start(0));
            let _ = writeln!(
                f,
                "pid {} since {}",
                std::process::id(),
                chrono::Utc::now().to_rfc3339()
            );
            let _ = f.flush();
            Ok(Lock { _file: file, path })
        }
        Ok(false) | Err(_) => {
            let hint = std::fs::read_to_string(&path)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "no owner recorded".to_string());
            Err(LockError::Held {
                path: path.display().to_string(),
                hint,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hajime-lock-{tag}-{}.lock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn a_second_caller_is_refused_while_the_first_holds_it() {
        // The point of the whole module: two schedulers must not both fire.
        // A second open of the same path conflicts even from this process,
        // because the lock belongs to the open file description, not the pid.
        let path = scratch("contention");
        let held = acquire(&path).expect("the first caller gets the lock");
        assert_eq!(held.path(), path.as_path());

        match acquire(&path) {
            Err(LockError::Held { .. }) => {}
            Err(e) => panic!("expected contention, got {e}"),
            Ok(_) => panic!("two processes must not hold this at once"),
        }

        drop(held);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_owner_is_recorded_for_whoever_reads_the_file() {
        // Read after releasing. While the lock is held this read succeeds on
        // FreeBSD, where flock is advisory, and fails on Windows, where the
        // lock is mandatory. The production code already treats an unreadable
        // file as "no owner recorded" rather than as an error, so this test
        // checks the content rather than the platform's locking semantics.
        let path = scratch("owner");
        drop(acquire(&path).expect("the lock should be free"));

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains(&std::process::id().to_string()),
            "the file should name the holder: {written}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn releasing_lets_the_next_caller_in() {
        // A restart must not be blocked by its own previous run. This is the
        // case a pid file gets wrong after a crash.
        let path = scratch("release");
        let first = acquire(&path).unwrap();
        drop(first);

        let second = acquire(&path).expect("the lock should be free again");
        drop(second);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unopenable_path_is_reported_as_such_not_as_contention() {
        // A permissions problem and a second instance need different fixes, so
        // they must not produce the same error.
        let path = PathBuf::from("").join("\0invalid");
        match acquire(&path) {
            Err(LockError::Unopenable { .. }) => {}
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("that path should not be lockable"),
        }
    }
}
