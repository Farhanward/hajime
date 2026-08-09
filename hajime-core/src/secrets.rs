//! Secret storage.
//!
//! Every pillar reads credentials through this one interface. The alternative
//! is each service inventing its own `.env` parsing, which is how the current
//! server ended up with tunnel tokens visible in `ps` output to any user.
//!
//! Three rules the design enforces rather than documents:
//!
//! 1. **Secrets never travel through the environment.** A value in the
//!    environment is readable from `ps` on some systems and lands in shell
//!    history, systemd units and crash dumps. Values are read from files.
//! 2. **A loose-permission file is refused, not warned about.** A secret any
//!    user can read is not a secret, and a warning nobody reads is not a guard.
//! 3. **A secret never lands in a log.** [`Secret`] prints as `<redacted>` in
//!    both `Debug` and `Display`, so it cannot leak through a stray format.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret '{0}' is not configured")]
    Missing(String),
    #[error("cannot read {path}: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is mode {mode:o}: readable beyond its owner. Run: chmod 600 {path}")]
    TooOpen { path: PathBuf, mode: u32 },
    #[error("{0} is empty")]
    Empty(PathBuf),
}

/// A secret value that will not print itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Read the value. Every call site is a place a secret could escape, so
    /// the name is deliberately awkward.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Secrets loaded from a directory, one file per secret.
///
/// A directory of files beats a single blob: permissions are per secret, a
/// rotation is one atomic file write, and nothing has to parse a format.
pub struct Store {
    values: HashMap<String, Secret>,
    root: Option<PathBuf>,
}

impl Store {
    pub fn empty() -> Self {
        Self { values: HashMap::new(), root: None }
    }

    /// Load every file in `dir` as a secret named after the file.
    ///
    /// Files whose permissions are too open are refused with an error rather
    /// than skipped, so a mistake is loud at startup instead of silent.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, SecretError> {
        let dir = dir.as_ref();
        let mut values = HashMap::new();

        let entries = std::fs::read_dir(dir).map_err(|source| SecretError::Unreadable {
            path: dir.to_path_buf(),
            source,
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // Editor leftovers are not secrets.
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) if !n.starts_with('.') && !n.ends_with('~') => n.to_string(),
                _ => continue,
            };
            values.insert(name, read_file(&path)?);
        }

        Ok(Self { values, root: Some(dir.to_path_buf()) })
    }

    /// Add a secret held in memory. For tests and for values that genuinely
    /// arrive at runtime, such as a token minted during an OAuth exchange.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.values.insert(name.into(), Secret::new(value));
    }

    pub fn get(&self, name: &str) -> Result<&Secret, SecretError> {
        self.values
            .get(name)
            .ok_or_else(|| SecretError::Missing(name.to_string()))
    }

    pub fn try_get(&self, name: &str) -> Option<&Secret> {
        self.values.get(name)
    }

    /// Names only. Safe to log, and the basis of a useful startup line.
    pub fn names(&self) -> Vec<&str> {
        let mut n: Vec<&str> = self.values.keys().map(String::as_str).collect();
        n.sort_unstable();
        n
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("secrets::Store")
            .field("count", &self.values.len())
            .field("names", &self.names())
            .finish()
    }
}

fn read_file(path: &Path) -> Result<Secret, SecretError> {
    check_permissions(path)?;

    let raw = std::fs::read_to_string(path).map_err(|source| SecretError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;

    // A trailing newline from an editor is not part of the secret.
    let value = raw.trim_end_matches(['\n', '\r']).to_string();
    if value.is_empty() {
        return Err(SecretError::Empty(path.to_path_buf()));
    }
    Ok(Secret::new(value))
}

#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|source| SecretError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = meta.permissions().mode() & 0o777;
    // Any permission for group or other is too much.
    if mode & 0o077 != 0 {
        return Err(SecretError::TooOpen { path: path.to_path_buf(), mode });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<(), SecretError> {
    // Windows ACLs do not map onto the unix mode bits, and this service is
    // deployed on FreeBSD. Development on Windows is not the threat model.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hajime_secrets_{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, name: &str, value: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, value).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }

    #[test]
    fn a_secret_never_prints_itself() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s}"), "<redacted>");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert!(!format!("{s} {s:?}").contains("hunter2"));
        // The value is still reachable when genuinely needed.
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn the_store_debug_shows_names_but_no_values() {
        let mut store = Store::empty();
        store.insert("cloudflare_token", "super-secret-value");
        let rendered = format!("{store:?}");
        assert!(rendered.contains("cloudflare_token"));
        assert!(!rendered.contains("super-secret-value"));
    }

    #[test]
    fn loads_one_secret_per_file() {
        let dir = temp_dir("load");
        write(&dir, "smtp_password", "p4ss");
        write(&dir, "api_token", "t0ken");

        let store = Store::from_dir(&dir).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.get("smtp_password").unwrap().expose(), "p4ss");
        assert_eq!(store.get("api_token").unwrap().expose(), "t0ken");
        assert_eq!(store.names(), vec!["api_token", "smtp_password"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trailing_newline_is_not_part_of_the_secret() {
        let dir = temp_dir("newline");
        write(&dir, "token", "value\n");
        let store = Store::from_dir(&dir).unwrap();
        assert_eq!(store.get("token").unwrap().expose(), "value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_secret_names_itself_in_the_error() {
        let store = Store::empty();
        let err = store.get("cloudflare_token").unwrap_err();
        assert!(err.to_string().contains("cloudflare_token"));
        assert!(store.try_get("cloudflare_token").is_none());
    }

    #[test]
    fn an_empty_file_is_an_error_not_an_empty_secret() {
        let dir = temp_dir("blank");
        write(&dir, "token", "");
        assert!(matches!(Store::from_dir(&dir), Err(SecretError::Empty(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dotfiles_and_editor_backups_are_ignored() {
        let dir = temp_dir("noise");
        write(&dir, "real", "value");
        write(&dir, ".hidden", "x");
        write(&dir, "backup~", "x");

        let store = Store::from_dir(&dir).unwrap();
        assert_eq!(store.names(), vec!["real"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_secret_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("open");
        let p = write(&dir, "token", "value");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();

        match Store::from_dir(&dir) {
            Err(SecretError::TooOpen { mode, .. }) => assert_eq!(mode, 0o644),
            other => panic!("a 0644 secret must be refused, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_reports_the_path() {
        let err = Store::from_dir("/nonexistent/hajime/secrets").unwrap_err();
        assert!(err.to_string().contains("hajime"));
    }
}
