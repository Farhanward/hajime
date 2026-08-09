//! The boot self-check.
//!
//! The system says what it found before it says it is ready. A machine that
//! announces "up" while the tunnel is down and last night's backup never ran
//! has told you nothing useful.
//!
//! Every check answers one question and reports one of three states. The
//! distinction between a failure and a warning is the whole point: a failure
//! means the sites are down, a warning means something needs attention this
//! week. Collapsing them would train the operator to ignore both.

use crate::service::{Service, SERVICES};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Pass,
    /// Working, but something will bite later.
    Warn,
    /// Not working. The system is not ready.
    Fail,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub state: State,
    pub detail: String,
}

impl Check {
    pub fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), state: State::Pass, detail: detail.into() }
    }
    pub fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), state: State::Warn, detail: detail.into() }
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), state: State::Fail, detail: detail.into() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
    pub ready: bool,
}

impl Report {
    pub fn new(checks: Vec<Check>) -> Self {
        let ready = !checks.iter().any(|c| c.state == State::Fail);
        Self { checks, ready }
    }

    pub fn counts(&self) -> (usize, usize, usize) {
        let p = self.checks.iter().filter(|c| c.state == State::Pass).count();
        let w = self.checks.iter().filter(|c| c.state == State::Warn).count();
        let f = self.checks.iter().filter(|c| c.state == State::Fail).count();
        (p, w, f)
    }

    /// A one-line verdict, for the console and for a notification.
    pub fn summary(&self) -> String {
        let (p, w, f) = self.counts();
        if self.ready && w == 0 {
            format!("ready: {p} checks passed")
        } else if self.ready {
            format!("ready with {w} warning(s): {p} passed")
        } else {
            format!("NOT READY: {f} failed, {w} warning(s), {p} passed")
        }
    }

    /// Rendered for a terminal, failures first because that is what is read.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut ordered: Vec<&Check> = self.checks.iter().collect();
        ordered.sort_by_key(|c| match c.state {
            State::Fail => 0,
            State::Warn => 1,
            State::Pass => 2,
        });
        for c in ordered {
            let mark = match c.state {
                State::Pass => "ok  ",
                State::Warn => "warn",
                State::Fail => "FAIL",
            };
            out.push_str(&format!("  {mark}  {:<22} {}\n", c.name, c.detail));
        }
        out.push_str(&format!("\n{}\n", self.summary()));
        out
    }
}

/// Is a TCP port accepting connections?
pub fn port_open(port: u16, timeout: Duration) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

/// Turn one service's observed state into a check.
///
/// Split out from [`check_services`] so the rule can be tested without opening
/// a socket. The test that used to cover this asked the operating system
/// whether ports 1 and 2 were closed, which is a claim about the whole machine
/// rather than about this rule, and it failed once on a loaded host when
/// something answered on port 2.
pub fn service_check(name: &'static str, port: u16, open: bool, essential: bool) -> Check {
    match (open, essential) {
        (true, _) => Check::pass(name, format!("listening on {port}")),
        // An optional service that is down is a choice, not a fault: saving
        // mode stops these on purpose.
        (false, false) => Check::warn(name, format!("not listening on {port}")),
        (false, true) => Check::fail(name, format!("not listening on {port}")),
    }
}

/// Check the services that declare a port.
pub fn check_services(services: &[Service]) -> Vec<Check> {
    services
        .iter()
        .filter_map(|s| {
            let port = s.port?;
            let open = port_open(port, Duration::from_millis(700));
            Some(service_check(s.name, port, open, s.is_essential()))
        })
        .collect()
}

/// Did a backup run recently enough to be worth having?
pub fn check_backup(path: &std::path::Path, max_age_hours: i64) -> Check {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            return Check::fail(
                "backup",
                format!("{} does not exist: nothing has been backed up", path.display()),
            )
        }
    };

    let modified = match meta.modified().ok().and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH).ok()
    }) {
        Some(d) => d.as_secs() as i64,
        None => return Check::warn("backup", "cannot read the backup timestamp"),
    };

    let age_hours = (chrono::Utc::now().timestamp() - modified) / 3600;
    if age_hours <= max_age_hours {
        Check::pass("backup", format!("{age_hours}h old"))
    } else {
        Check::fail(
            "backup",
            format!("{age_hours}h old, older than the {max_age_hours}h limit"),
        )
    }
}

/// How close a TLS certificate is to expiring.
///
/// Takes the days rather than reading the certificate, so the parsing lives
/// with whatever already knows how to read it and this stays testable.
pub fn check_certificate(name: &str, days_left: i64) -> Check {
    match days_left {
        d if d < 0 => Check::fail(name, format!("expired {} days ago", -d)),
        d if d < 7 => Check::fail(name, format!("expires in {d} days")),
        d if d < 30 => Check::warn(name, format!("expires in {d} days")),
        d => Check::pass(name, format!("valid for {d} days")),
    }
}

/// Run the standard set.
pub fn run(backup_path: Option<&std::path::Path>) -> Report {
    let mut checks = check_services(SERVICES);
    if let Some(p) = backup_path {
        checks.push(check_backup(p, 26));
    } else {
        checks.push(Check::warn("backup", "no backup path configured"));
    }
    if crate::snapshot::boot_environments_available() {
        checks.push(Check::pass("rollback", "boot environments available"));
    } else {
        checks.push(Check::warn(
            "rollback",
            "bectl unavailable: there is no one-step way back from a bad change",
        ));
    }
    Report::new(checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::Tier;

    fn svc(name: &'static str, port: u16, tier: Tier) -> Service {
        Service {
            name,
            rc_script: None,
            description: "test",
            tier,
            port: Some(port),
            health_path: Some("/health"),
            typical_mb: 10,
        }
    }

    #[test]
    fn a_failure_makes_the_system_not_ready() {
        let r = Report::new(vec![
            Check::pass("a", "fine"),
            Check::fail("b", "broken"),
        ]);
        assert!(!r.ready);
        assert!(r.summary().starts_with("NOT READY"));
    }

    #[test]
    fn warnings_alone_still_count_as_ready() {
        // The distinction is the point: a warning must not block the boot, and
        // must not be invisible either.
        let r = Report::new(vec![Check::pass("a", "fine"), Check::warn("b", "soon")]);
        assert!(r.ready);
        assert!(r.summary().contains("warning"));
    }

    #[test]
    fn an_essential_service_that_is_down_fails_but_an_optional_one_warns() {
        // No socket involved. The earlier version of this test assumed ports 1
        // and 2 were closed on whatever machine it ran on, and on a loaded
        // FreeBSD host something answered on port 2 and the test failed for a
        // reason that had nothing to do with this rule.
        assert_eq!(service_check("essential", 5432, false, true).state, State::Fail);
        assert_eq!(
            service_check("optional", 11434, false, false).state,
            State::Warn,
            "saving mode stops these on purpose"
        );
        // Running is running, whichever tier it belongs to.
        assert_eq!(service_check("essential", 5432, true, true).state, State::Pass);
        assert_eq!(service_check("optional", 11434, true, false).state, State::Pass);
    }

    #[test]
    fn the_detail_names_the_port_so_the_operator_knows_where_to_look() {
        let c = service_check("caddy", 80, false, true);
        assert!(c.detail.contains("80"), "{}", c.detail);
    }

    #[test]
    fn port_open_agrees_with_a_socket_we_control() {
        // Deterministic in both directions: bind a real listener and ask, then
        // drop it and ask again. Port 0 lets the OS pick one that is genuinely
        // free, rather than guessing at a low number and hoping.
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the loopback should bind");
        let port = listener.local_addr().unwrap().port();
        assert!(
            port_open(port, Duration::from_millis(700)),
            "a bound port should read as open"
        );

        drop(listener);
        assert!(
            !port_open(port, Duration::from_millis(700)),
            "a released port should read as closed"
        );
    }

    #[test]
    fn a_service_with_no_port_is_not_checked() {
        // cloudflared has no port to poll. Reporting it as down would put a
        // permanent false failure on the boot check.
        let mut s = svc("tunnel", 0, Tier::Essential);
        s.port = None;
        assert!(check_services(&[s]).is_empty());
    }

    #[test]
    fn a_missing_backup_is_a_failure_not_a_warning() {
        let c = check_backup(std::path::Path::new("/definitely/not/here.tar.gz"), 26);
        assert_eq!(c.state, State::Fail);
        assert!(c.detail.contains("nothing has been backed up"));
    }

    #[test]
    fn a_fresh_backup_passes_and_a_stale_one_fails() {
        let p = std::env::temp_dir().join("hajime_readiness_backup.txt");
        std::fs::write(&p, b"x").unwrap();

        assert_eq!(check_backup(&p, 26).state, State::Pass);
        // The same file judged against a zero-hour limit is stale.
        assert_eq!(check_backup(&p, -1).state, State::Fail);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn certificate_thresholds_escalate_as_expiry_approaches() {
        assert_eq!(check_certificate("tls", 90).state, State::Pass);
        assert_eq!(check_certificate("tls", 29).state, State::Warn);
        assert_eq!(check_certificate("tls", 6).state, State::Fail);
        assert_eq!(check_certificate("tls", -2).state, State::Fail);
        assert!(check_certificate("tls", -2).detail.contains("expired"));
    }

    #[test]
    fn the_rendering_puts_failures_first() {
        let r = Report::new(vec![
            Check::pass("a", "fine"),
            Check::warn("b", "soon"),
            Check::fail("c", "broken"),
        ]);
        let text = r.render();
        let fail_at = text.find("FAIL").unwrap();
        let warn_at = text.find("warn").unwrap();
        let ok_at = text.find("ok  ").unwrap();
        assert!(fail_at < warn_at && warn_at < ok_at, "failures must be read first");
    }

    #[test]
    fn a_service_without_a_port_is_skipped_rather_than_guessed_at() {
        let tunnel = Service {
            name: "cloudflared",
            rc_script: None,
            description: "tunnel",
            tier: Tier::Essential,
            port: None,
            health_path: None,
            typical_mb: 50,
        };
        assert!(check_services(&[tunnel]).is_empty());
    }
}
