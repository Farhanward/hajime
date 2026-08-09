//! What the engine is permitted to do with the host.
//!
//! Three node kinds reach outside the process: `executeCommand` runs a shell,
//! `ssh` runs one on another machine, and `readWriteFile` touches the disk.
//! In the imported workflows two of them take their command straight from a
//! webhook body (`"command": "={{ $json.body.cmd }}"`), which is remote code
//! execution by design. Those workflows are disabled today, and nothing here
//! should quietly make them live again.
//!
//! So the capabilities exist but are off unless switched on, and file access
//! is confined to declared roots. The default is a service that can run every
//! active workflow and nothing more.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Policy {
    /// Allow the `executeCommand` node to run a shell on this host.
    pub allow_command: bool,
    /// Allow the `ssh` node to run a shell on another host.
    pub allow_ssh: bool,
    /// Directories `readWriteFile` may work inside. Empty denies all file access.
    pub file_roots: Vec<PathBuf>,
    /// Ceiling on a single command's runtime, in seconds.
    pub command_timeout_secs: u64,
    /// Ceiling on bytes read from a file or captured from a command.
    pub max_bytes: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_command: false,
            allow_ssh: false,
            file_roots: Vec::new(),
            command_timeout_secs: 120,
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

impl Policy {
    /// Read the policy from the environment.
    ///
    /// `HAJIME_ALLOW_COMMAND=1`, `HAJIME_ALLOW_SSH=1`,
    /// `HAJIME_FILE_ROOTS=/vault/secrets:/vault/reports`
    pub fn from_env() -> Self {
        let flag = |name: &str| {
            matches!(
                std::env::var(name).unwrap_or_default().as_str(),
                "1" | "true" | "yes"
            )
        };
        let roots = std::env::var("HAJIME_FILE_ROOTS")
            .unwrap_or_default()
            .split([':', ';'])
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .collect();

        Self {
            allow_command: flag("HAJIME_ALLOW_COMMAND"),
            allow_ssh: flag("HAJIME_ALLOW_SSH"),
            file_roots: roots,
            ..Self::default()
        }
    }

    /// Everything on. Gated behind the `testing` feature so it cannot be
    /// reached from a release build by accident: this policy permits shell
    /// execution and unrestricted file access.
    #[cfg(feature = "testing")]
    pub fn permissive(roots: Vec<PathBuf>) -> Self {
        Self {
            allow_command: true,
            allow_ssh: true,
            file_roots: roots,
            ..Self::default()
        }
    }

    /// Is `path` inside a declared root?
    ///
    /// Compared after normalisation so `/vault/secrets/../../etc/passwd` cannot
    /// walk out of an allowed directory. The path need not exist yet, which
    /// matters because a write may be creating it.
    pub fn file_allowed(&self, path: &Path) -> bool {
        if self.file_roots.is_empty() {
            return false;
        }
        let target = normalise(path);
        self.file_roots
            .iter()
            .any(|root| target.starts_with(normalise(root)))
    }
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_dangerous_is_off_by_default() {
        let p = Policy::default();
        assert!(!p.allow_command);
        assert!(!p.allow_ssh);
        assert!(p.file_roots.is_empty());
        assert!(!p.file_allowed(Path::new("/vault/secrets/x.js")));
    }

    #[test]
    fn a_path_inside_a_root_is_allowed() {
        let p = Policy::permissive(vec![PathBuf::from("/vault/secrets")]);
        assert!(p.file_allowed(Path::new("/vault/secrets/totp-api-server.js")));
        assert!(p.file_allowed(Path::new("/vault/secrets/nested/deep.txt")));
    }

    #[test]
    fn a_path_outside_every_root_is_refused() {
        let p = Policy::permissive(vec![PathBuf::from("/vault/secrets")]);
        assert!(!p.file_allowed(Path::new("/etc/passwd")));
        assert!(!p.file_allowed(Path::new("/vault/other/x")));
    }

    #[test]
    fn traversal_cannot_escape_a_root() {
        let p = Policy::permissive(vec![PathBuf::from("/vault/secrets")]);
        assert!(!p.file_allowed(Path::new("/vault/secrets/../../etc/passwd")));
        assert!(!p.file_allowed(Path::new("/vault/secrets/../other/x")));
        // Staying inside after a detour is still inside.
        assert!(p.file_allowed(Path::new("/vault/secrets/a/../b.txt")));
    }

    #[test]
    fn no_roots_means_no_file_access_at_all() {
        let p = Policy::permissive(vec![]);
        assert!(!p.file_allowed(Path::new("/anything")));
    }
}
