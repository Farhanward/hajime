//! Service configuration.
//!
//! Settings come from the environment; secrets do not. The environment is a
//! reasonable place for a bind address or a timezone, and a poor place for a
//! token: it is inherited by children, visible in `ps` on some systems, and
//! ends up in crash dumps and process listings. Secrets are read from files by
//! [`crate::secrets`], and the environment only says *where*.
//!
//! Every pillar reads `HAJIME_*` for shared settings and `HAJIME_<PILLAR>_*`
//! for its own, so two services can never collide on a name.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{key} is not a valid address: {value}")]
    BadAddress { key: String, value: String },
    #[error("{key} must be set")]
    Required { key: String },
    #[error(
        "refusing to bind {addr} without a token: the control endpoints can \
         reach every system this service touches. Set {token_key}, or bind to \
         127.0.0.1."
    )]
    ExposedWithoutToken { addr: SocketAddr, token_key: String },
}

/// Settings shared by every pillar.
#[derive(Debug, Clone)]
pub struct Common {
    pub bind: SocketAddr,
    pub secrets_dir: Option<PathBuf>,
    pub history_path: Option<PathBuf>,
    pub timezone: String,
}

impl Common {
    /// Read settings for a pillar. `prefix` is the pillar's own namespace, so
    /// `Common::from_env("WORKFLOW", "127.0.0.1:5678")` reads
    /// `HAJIME_WORKFLOW_BIND` first and falls back to `HAJIME_BIND`.
    ///
    /// `default_bind` is a parameter rather than a constant here because this
    /// function serves every pillar. It used to hardcode the workflow engine's
    /// port, so the console, the gateway and the WhatsApp bridge all defaulted
    /// onto 5678: the console documented 8088, bound to 5678, and would have
    /// fought the engine for it on a machine where both ran.
    pub fn from_env(prefix: &str, default_bind: &str) -> Result<Self, ConfigError> {
        let bind_key = format!("HAJIME_{prefix}_BIND");
        let raw_bind = var(&bind_key)
            .or_else(|| var("HAJIME_BIND"))
            .unwrap_or_else(|| default_bind.to_string());

        let bind = raw_bind
            .parse()
            .map_err(|_| ConfigError::BadAddress { key: bind_key, value: raw_bind })?;

        Ok(Self {
            bind,
            secrets_dir: var(&format!("HAJIME_{prefix}_SECRETS"))
                .or_else(|| var("HAJIME_SECRETS"))
                .map(PathBuf::from),
            history_path: var(&format!("HAJIME_{prefix}_HISTORY"))
                .or_else(|| var("HAJIME_HISTORY"))
                .map(PathBuf::from),
            // The workflows this system inherits were written against a host
            // running Asia/Riyadh. Defaulting to UTC would shift every
            // schedule by three hours while appearing to work.
            timezone: var("HAJIME_TZ").unwrap_or_else(|| "Asia/Riyadh".to_string()),
        })
    }

    /// Refuse to listen beyond loopback without authentication.
    ///
    /// Checked here rather than left to each pillar, so no service can be
    /// exposed by forgetting the check.
    pub fn guard_exposure(&self, has_token: bool, token_key: &str) -> Result<(), ConfigError> {
        if self.bind.ip().is_loopback() || has_token {
            return Ok(());
        }
        Err(ConfigError::ExposedWithoutToken {
            addr: self.bind,
            token_key: token_key.to_string(),
        })
    }

    pub fn is_loopback(&self) -> bool {
        self.bind.ip().is_loopback()
    }
}

/// An environment variable, treating blank as absent.
fn var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Read a boolean flag. Only explicit affirmatives count, so a typo disables
/// a capability rather than enabling one.
pub fn flag(key: &str) -> bool {
    matches!(
        var(key).unwrap_or_default().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment variables are process-global, so these tests share a lock
    /// rather than racing each other under the test harness.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn clear() {
        for k in [
            "HAJIME_BIND", "HAJIME_WF_BIND", "HAJIME_SECRETS", "HAJIME_WF_SECRETS",
            "HAJIME_HISTORY", "HAJIME_WF_HISTORY", "HAJIME_TZ", "HAJIME_TEST_FLAG",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn each_pillar_gets_the_port_it_asked_for() {
        // The bug this replaces: from_env hardcoded 5678 for everyone, so the
        // console documented 8088 and bound to the workflow engine's port. It
        // only showed up by starting the console and reading the log line.
        let _guard = lock();
        clear();
        // Not in clear()'s list, and a stale value would make this pass for
        // the wrong reason.
        std::env::remove_var("HAJIME_CONSOLE_BIND");
        std::env::remove_var("HAJIME_AI_BIND");
        let console = Common::from_env("CONSOLE", "127.0.0.1:8088").unwrap();
        assert_eq!(console.bind.to_string(), "127.0.0.1:8088");

        let ai = Common::from_env("AI", "127.0.0.1:11434").unwrap();
        assert_eq!(ai.bind.to_string(), "127.0.0.1:11434");

        // Two pillars must not land on the same port by default, which is the
        // shape of the original fault.
        assert_ne!(console.bind.port(), ai.bind.port());
    }

    #[test]
    fn defaults_are_loopback_and_riyadh() {
        let _g = lock();
        clear();
        let c = Common::from_env("WF", "127.0.0.1:5678").unwrap();
        assert_eq!(c.bind.to_string(), "127.0.0.1:5678");
        assert_eq!(c.timezone, "Asia/Riyadh");
        assert!(c.is_loopback());
        assert!(c.secrets_dir.is_none());
    }

    #[test]
    fn the_pillar_namespace_wins_over_the_shared_one() {
        let _g = lock();
        clear();
        std::env::set_var("HAJIME_BIND", "127.0.0.1:1111");
        std::env::set_var("HAJIME_WF_BIND", "127.0.0.1:2222");
        let c = Common::from_env("WF", "127.0.0.1:5678").unwrap();
        assert_eq!(c.bind.port(), 2222);
        clear();
    }

    #[test]
    fn a_blank_value_counts_as_unset() {
        let _g = lock();
        clear();
        std::env::set_var("HAJIME_BIND", "   ");
        let c = Common::from_env("WF", "127.0.0.1:5678").unwrap();
        assert_eq!(c.bind.to_string(), "127.0.0.1:5678");
        clear();
    }

    #[test]
    fn a_malformed_address_names_the_key_that_set_it() {
        let _g = lock();
        clear();
        std::env::set_var("HAJIME_WF_BIND", "not-an-address");
        let err = Common::from_env("WF", "127.0.0.1:5678").unwrap_err();
        assert!(err.to_string().contains("HAJIME_WF_BIND"), "got: {err}");
        clear();
    }

    #[test]
    fn binding_beyond_loopback_needs_a_token() {
        let _g = lock();
        clear();
        std::env::set_var("HAJIME_BIND", "0.0.0.0:5678");
        let c = Common::from_env("WF", "127.0.0.1:5678").unwrap();

        let err = c.guard_exposure(false, "HAJIME_TOKEN").unwrap_err();
        assert!(err.to_string().contains("HAJIME_TOKEN"), "got: {err}");
        // With a token it is allowed.
        assert!(c.guard_exposure(true, "HAJIME_TOKEN").is_ok());
        clear();
    }

    #[test]
    fn loopback_never_needs_a_token() {
        let _g = lock();
        clear();
        let c = Common::from_env("WF", "127.0.0.1:5678").unwrap();
        assert!(c.guard_exposure(false, "HAJIME_TOKEN").is_ok());
    }

    #[test]
    fn only_explicit_affirmatives_enable_a_flag() {
        let _g = lock();
        clear();
        for yes in ["1", "true", "TRUE", "yes", "on"] {
            std::env::set_var("HAJIME_TEST_FLAG", yes);
            assert!(flag("HAJIME_TEST_FLAG"), "{yes} should enable");
        }
        // A typo must disable a capability, never enable one.
        for no in ["0", "false", "no", "off", "ture", "y", ""] {
            std::env::set_var("HAJIME_TEST_FLAG", no);
            assert!(!flag("HAJIME_TEST_FLAG"), "{no:?} must not enable");
        }
        std::env::remove_var("HAJIME_TEST_FLAG");
        assert!(!flag("HAJIME_TEST_FLAG"));
        clear();
    }
}
