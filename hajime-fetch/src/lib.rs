//! Fetching pages for the model.
//!
//! What replaced the browser. It does not render, and it says so: a page whose
//! content is assembled by JavaScript comes back flagged rather than empty.
//! Everything else — articles, feeds, documentation, APIs — is a parsing job
//! that costs kilobytes.
//!
//! The URL here is chosen by the model, which makes this the one tool where a
//! bad prompt becomes a network request. So the address is checked before the
//! connection: private ranges, loopback and the cloud metadata endpoint are
//! refused. Without that, "summarise http://169.254.169.254/..." is a
//! credential leak wearing a research question.

pub mod extract;

use async_trait::async_trait;
use extract::Page;
use hajime_core::tools::{ToolProvider, ToolSpec};
use std::net::IpAddr;
use std::time::Duration;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum FetchError {
    #[error("only http and https are allowed, not '{0}'")]
    BadScheme(String),
    #[error("'{0}' is not a usable URL")]
    BadUrl(String),
    #[error("{host} resolves to {addr}, which is not on the public internet")]
    PrivateAddress { host: String, addr: String },
    #[error("could not reach {url}: {reason}")]
    Unreachable { url: String, reason: String },
    #[error("{url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url} returned {bytes} bytes, above the {limit} byte limit")]
    TooLarge { url: String, bytes: usize, limit: usize },
}

/// Is this address somewhere a model should never be able to reach?
///
/// Loopback, link-local and the private ranges cover the internal network and
/// the machine itself. `169.254.169.254` is called out because it is the cloud
/// metadata service and its whole purpose is handing out credentials.
pub fn is_private(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // 100.64.0.0/10, carrier-grade NAT.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // fc00::/7 unique local, fe80::/10 link local.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

fn host_of(url: &str) -> Result<String, FetchError> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| {
            let scheme = url.split(':').next().unwrap_or(url).to_string();
            FetchError::BadScheme(scheme)
        })?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split('@')
        .next_back()
        .unwrap_or_default();

    // An IPv6 literal is bracketed and full of colons, so the port cannot be
    // found by splitting on ':'. Getting this wrong let `http://[::1]/` past
    // the private-address check entirely.
    let host = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((inside, _port)) => inside,
            None => return Err(FetchError::BadUrl(url.to_string())),
        }
    } else {
        authority.split(':').next().unwrap_or_default()
    };

    if host.is_empty() {
        return Err(FetchError::BadUrl(url.to_string()));
    }
    Ok(host.to_string())
}

/// Refuse a URL before opening a connection to it.
pub fn check_url(url: &str) -> Result<String, FetchError> {
    let host = host_of(url)?;

    // A literal address needs no lookup and must be checked directly.
    if let Ok(addr) = host.parse::<IpAddr>() {
        if is_private(&addr) {
            return Err(FetchError::PrivateAddress {
                host: host.clone(),
                addr: addr.to_string(),
            });
        }
        return Ok(host);
    }

    // A name may still point inside. Resolve and check every answer: a host
    // that returns one public and one private address must be refused.
    use std::net::ToSocketAddrs;
    match (host.as_str(), 80u16).to_socket_addrs() {
        Ok(addrs) => {
            for sa in addrs {
                if is_private(&sa.ip()) {
                    return Err(FetchError::PrivateAddress {
                        host: host.clone(),
                        addr: sa.ip().to_string(),
                    });
                }
            }
            Ok(host)
        }
        // A name that does not resolve fails at connect time with a clearer
        // message than anything invented here.
        Err(_) => Ok(host),
    }
}

pub struct Fetcher {
    client: reqwest::Client,
    max_bytes: usize,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("hajime-fetch/1.0")
                // Redirects are where an allowed URL becomes a private one.
                // Each hop is re-checked below rather than followed blindly.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("a default reqwest client is always valid"),
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

impl Fetcher {
    pub async fn get(&self, url: &str) -> Result<Page, FetchError> {
        let mut current = url.to_string();

        // Follow redirects by hand so every hop passes the same address check.
        for _ in 0..5 {
            check_url(&current)?;
            let response = self.client.get(&current).send().await.map_err(|e| {
                FetchError::Unreachable { url: current.clone(), reason: e.to_string() }
            })?;

            let status = response.status();
            if status.is_redirection() {
                let next = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(|l| extract_resolve(&current, l))
                    .ok_or_else(|| FetchError::Status {
                        url: current.clone(),
                        status: status.as_u16(),
                    })?;
                current = next;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::Status { url: current, status: status.as_u16() });
            }

            let bytes = response.bytes().await.map_err(|e| FetchError::Unreachable {
                url: current.clone(),
                reason: e.to_string(),
            })?;
            if bytes.len() > self.max_bytes {
                return Err(FetchError::TooLarge {
                    url: current,
                    bytes: bytes.len(),
                    limit: self.max_bytes,
                });
            }

            let html = String::from_utf8_lossy(&bytes);
            return Ok(extract::extract(&current, &html));
        }

        Err(FetchError::Unreachable {
            url: current,
            reason: "too many redirects".into(),
        })
    }
}

/// Resolve a `Location` header against the URL it came from.
fn extract_resolve(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let Some(scheme_end) = base.find("://") else { return location.to_string() };
    let after = &base[scheme_end + 3..];
    let host_end = after.find('/').map(|i| scheme_end + 3 + i).unwrap_or(base.len());
    if location.starts_with('/') {
        format!("{}{location}", &base[..host_end])
    } else {
        format!("{}/{location}", &base[..host_end])
    }
}

/// The `fetch_page` tool.
#[derive(Default)]
pub struct Fetch {
    fetcher: Fetcher,
}


#[async_trait]
impl ToolProvider for Fetch {
    fn name(&self) -> &str {
        "fetch"
    }

    async fn list(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::read(
            "fetch_page",
            "Fetch a web page and return its readable text, title and links. \
             Does not run JavaScript: a page built entirely by scripts comes \
             back with needsJavaScript set and little text. Only public http \
             and https addresses are allowed.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "An http or https URL" }
                },
                "required": ["url"],
            }),
        )]
    }

    async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        if tool != "fetch_page" {
            return Err(format!("no tool named '{tool}'"));
        }
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "the 'url' argument is required".to_string())?;

        match self.fetcher.get(url).await {
            Ok(page) => {
                let mut value = serde_json::to_value(&page).unwrap_or_default();
                if page.needs_javascript {
                    // Say it in the payload as well as the flag: the model
                    // reads prose more reliably than it reads booleans.
                    value["note"] = serde_json::json!(
                        "This page is assembled by JavaScript and could not be \
                         read. Do not summarise it as if it were empty."
                    );
                }
                Ok(value)
            }
            Err(e @ FetchError::PrivateAddress { .. }) => {
                Err(format!("{e}. Only public addresses may be fetched."))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_and_private_ranges_are_refused() {
        for url in [
            "http://127.0.0.1/admin",
            "http://localhost:5678/status",
            "http://192.168.1.50/",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://[::1]/",
        ] {
            assert!(check_url(url).is_err(), "{url} must be refused");
        }
    }

    #[test]
    fn the_cloud_metadata_address_is_refused() {
        // The single most valuable target for a prompt-injected fetch.
        let err = check_url("http://169.254.169.254/latest/meta-data/").unwrap_err();
        assert!(matches!(err, FetchError::PrivateAddress { .. }));
    }

    #[test]
    fn a_public_address_is_allowed() {
        assert_eq!(check_url("https://example.com/a/b").unwrap(), "example.com");
        assert_eq!(check_url("https://8.8.8.8/").unwrap(), "8.8.8.8");
    }

    #[test]
    fn non_http_schemes_are_refused() {
        for url in ["file:///etc/passwd", "ftp://x/", "gopher://x/"] {
            assert!(matches!(check_url(url), Err(FetchError::BadScheme(_))), "{url}");
        }
    }

    #[test]
    fn credentials_in_the_url_do_not_hide_the_host() {
        // `http://example.com@127.0.0.1/` points at loopback, not example.com.
        let err = check_url("http://example.com@127.0.0.1/").unwrap_err();
        assert!(matches!(err, FetchError::PrivateAddress { .. }), "got {err:?}");
    }

    #[test]
    fn bracketed_ipv6_literals_are_parsed_and_refused() {
        // Splitting the authority on ':' turns `[::1]` into `[`, which parses
        // as no address at all and slipped straight past the check.
        for url in [
            "http://[::1]/",
            "http://[::1]:8080/admin",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
        ] {
            assert!(check_url(url).is_err(), "{url} must be refused");
        }
        // A public IPv6 address is still allowed.
        assert_eq!(
            check_url("https://[2606:4700::1111]/x").unwrap(),
            "2606:4700::1111"
        );
    }

    #[test]
    fn an_unterminated_bracket_is_refused_rather_than_guessed_at() {
        assert!(matches!(check_url("http://[::1/"), Err(FetchError::BadUrl(_))));
    }

    #[test]
    fn a_port_does_not_confuse_the_host_check() {
        assert!(check_url("http://127.0.0.1:8080/").is_err());
        assert_eq!(check_url("https://example.com:8443/x").unwrap(), "example.com");
    }

    #[test]
    fn private_ranges_are_classified_correctly() {
        let private: Vec<IpAddr> = vec![
            "127.0.0.1".parse().unwrap(),
            "10.1.2.3".parse().unwrap(),
            "192.168.1.1".parse().unwrap(),
            "172.20.0.1".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
            "100.64.0.1".parse().unwrap(),
            "::1".parse().unwrap(),
            "fd00::1".parse().unwrap(),
        ];
        for a in private {
            assert!(is_private(&a), "{a} should be private");
        }
        for a in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(!is_private(&a.parse().unwrap()), "{a} should be public");
        }
    }

    #[test]
    fn a_redirect_location_resolves_against_its_source() {
        assert_eq!(
            extract_resolve("https://a.test/x/y", "/z"),
            "https://a.test/z"
        );
        assert_eq!(
            extract_resolve("https://a.test/x", "https://b.test/q"),
            "https://b.test/q"
        );
    }

    #[tokio::test]
    async fn the_tool_refuses_a_private_url_with_an_actionable_message() {
        let f = Fetch::default();
        let err = f
            .call("fetch_page", serde_json::json!({"url": "http://192.168.1.50/"}))
            .await
            .unwrap_err();
        assert!(err.contains("not on the public internet"), "got: {err}");
        assert!(err.contains("Only public addresses"), "got: {err}");
    }

    #[tokio::test]
    async fn the_tool_requires_a_url() {
        let f = Fetch::default();
        assert!(f.call("fetch_page", serde_json::json!({})).await.is_err());
    }

    #[tokio::test]
    async fn fetching_is_declared_as_a_read() {
        // A read may run in dry-run mode; if this were mislabelled External
        // the model would lose its ability to research during a rehearsal.
        let specs = Fetch::default().list().await;
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].effect, hajime_core::tools::Effect::Read);
        assert!(specs[0].description.contains("Does not run JavaScript"));
    }
}
