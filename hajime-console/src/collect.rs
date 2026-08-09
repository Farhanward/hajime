//! Gathering what the other services know.
//!
//! The console owns no data. It asks each service and assembles one answer,
//! because the alternative is a database that has to be kept in step with
//! nine processes and is wrong the moment one of them restarts.
//!
//! A service that does not answer is reported as unreachable, never as
//! healthy-with-no-data. Those look identical on a dashboard and mean opposite
//! things.

use hajime_sys::service::{self, Service};
use serde::Serialize;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Reach {
    Up,
    Down,
    /// Listening, but its health endpoint answered with an error.
    Unhealthy,
    /// Nothing to poll: no port, or no health path.
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceView {
    pub name: &'static str,
    pub description: &'static str,
    pub essential: bool,
    pub port: Option<u16>,
    pub typical_mb: u32,
    pub reach: Reach,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub struct Collector {
    client: reqwest::Client,
}

impl Default for Collector {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                // Short: the console is polled by a browser and a slow service
                // must not make the page hang.
                .timeout(Duration::from_secs(3))
                .build()
                .expect("a default reqwest client is always valid"),
        }
    }
}

impl Collector {
    /// The state of every known service.
    pub async fn services(&self) -> Vec<ServiceView> {
        let mut out = Vec::new();
        for s in service::start_order() {
            out.push(self.one(s).await);
        }
        out
    }

    async fn one(&self, s: &'static Service) -> ServiceView {
        let (reach, detail) = match (s.port, s.health_url()) {
            // A health endpoint is the strongest signal: it means the process
            // is not merely bound to a port but answering.
            (_, Some(url)) => match self.client.get(&url).send().await {
                Ok(r) if r.status().is_success() => (Reach::Up, None),
                Ok(r) => (Reach::Unhealthy, Some(format!("HTTP {}", r.status().as_u16()))),
                Err(_) => (Reach::Down, None),
            },
            // A port with no health path: the best available answer is whether
            // something accepts a connection.
            (Some(port), None) => {
                if hajime_sys::readiness::port_open(port, Duration::from_millis(500)) {
                    (Reach::Up, Some("port open".into()))
                } else {
                    (Reach::Down, None)
                }
            }
            (None, None) => (Reach::Unknown, Some("nothing to poll".into())),
        };

        ServiceView {
            name: s.name,
            description: s.description,
            essential: s.is_essential(),
            port: s.port,
            typical_mb: s.typical_mb,
            reach,
            detail,
        }
    }

    /// Recent workflow runs, from the workflow service.
    pub async fn workflow_history(&self, base: &str, token: Option<&str>) -> Fetched {
        self.get_json(&format!("{}/api/history", base.trim_end_matches('/')), token)
            .await
    }

    /// Recent tool calls, from the model gateway.
    pub async fn tool_audit(&self, base: &str, token: Option<&str>) -> Fetched {
        self.get_json(&format!("{}/v1/audit", base.trim_end_matches('/')), token)
            .await
    }

    async fn get_json(&self, url: &str, token: Option<&str>) -> Fetched {
        let mut req = self.client.get(url);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                Ok(v) => Fetched::ok(v),
                Err(e) => Fetched::error(format!("unreadable response: {e}")),
            },
            Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => {
                Fetched::error("unauthorised: the console needs that service's token")
            }
            Ok(r) => Fetched::error(format!("HTTP {}", r.status().as_u16())),
            Err(_) => Fetched::Unreachable,
        }
    }
}

/// The result of asking another service for something.
///
/// `Unreachable` is kept separate from `Error` on purpose: one means the
/// service is not running, the other means it is running and refused. A
/// dashboard that renders both as an empty panel teaches the operator nothing.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Fetched {
    // Struct variants rather than newtypes: serde's internally-tagged
    // representation cannot serialise a variant holding a bare string.
    Ok { data: serde_json::Value },
    Error { message: String },
    Unreachable,
}

impl Fetched {
    pub fn ok(data: serde_json::Value) -> Self {
        Self::Ok { data }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error { message: message.into() }
    }
}

impl Fetched {
    pub fn value(&self) -> Option<&serde_json::Value> {
        match self {
            Fetched::Ok { data } => Some(data),
            _ => None,
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, Fetched::Ok { .. })
    }
}

/// A one-line verdict for the top of the page.
pub fn headline(views: &[ServiceView]) -> String {
    let down_essential: Vec<&str> = views
        .iter()
        .filter(|v| v.essential && v.reach == Reach::Down)
        .map(|v| v.name)
        .collect();
    let unhealthy: Vec<&str> = views
        .iter()
        .filter(|v| v.reach == Reach::Unhealthy)
        .map(|v| v.name)
        .collect();

    if !down_essential.is_empty() {
        format!("{} down: {}", down_essential.len(), down_essential.join(", "))
    } else if !unhealthy.is_empty() {
        format!("{} unhealthy: {}", unhealthy.len(), unhealthy.join(", "))
    } else {
        let optional_down = views
            .iter()
            .filter(|v| !v.essential && v.reach == Reach::Down)
            .count();
        if optional_down > 0 {
            format!("sites up, {optional_down} optional service(s) stopped")
        } else {
            "everything up".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(name: &'static str, essential: bool, reach: Reach) -> ServiceView {
        ServiceView {
            name,
            description: "test",
            essential,
            port: Some(1234),
            typical_mb: 10,
            reach,
            detail: None,
        }
    }

    #[test]
    fn an_essential_service_down_dominates_the_headline() {
        let views = vec![
            view("caddy", true, Reach::Down),
            view("llamacpp", false, Reach::Down),
        ];
        let h = headline(&views);
        assert!(h.contains("caddy"), "{h}");
        assert!(!h.contains("llamacpp"), "the essential failure is the story: {h}");
    }

    #[test]
    fn optional_services_stopped_reads_as_saving_mode_not_as_breakage() {
        let views = vec![
            view("caddy", true, Reach::Up),
            view("llamacpp", false, Reach::Down),
        ];
        assert_eq!(headline(&views), "sites up, 1 optional service(s) stopped");
    }

    #[test]
    fn everything_up_says_so_plainly() {
        let views = vec![view("caddy", true, Reach::Up), view("llamacpp", false, Reach::Up)];
        assert_eq!(headline(&views), "everything up");
    }

    #[test]
    fn unhealthy_is_reported_even_when_nothing_is_fully_down() {
        // Listening but answering 500 is worse than stopped, because it looks
        // fine to anything that only checks the port.
        let views = vec![view("hajime_workflow", true, Reach::Unhealthy)];
        assert!(headline(&views).contains("unhealthy"));
    }

    #[test]
    fn unreachable_and_error_are_distinct() {
        // One means the service is not running; the other means it is running
        // and refused. Rendering both as an empty panel teaches nothing.
        assert!(!Fetched::Unreachable.is_ok());
        assert!(!Fetched::error("401").is_ok());
        assert!(Fetched::ok(serde_json::json!({})).is_ok());

        let a = serde_json::to_string(&Fetched::Unreachable).unwrap();
        let b = serde_json::to_string(&Fetched::error("x")).unwrap();
        assert_ne!(a, b);
        assert!(a.contains("unreachable"));
    }

    #[test]
    fn a_fetched_value_is_reachable_only_when_ok() {
        assert!(Fetched::ok(serde_json::json!({"a": 1})).value().is_some());
        assert!(Fetched::Unreachable.value().is_none());
        assert!(Fetched::error("x").value().is_none());
    }
}
