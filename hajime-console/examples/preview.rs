//! Render the console page to a standalone file, to look at it.
//!
//! Run with: cargo run -p hajime-console --example preview -- <output.html>
//!
//! The real page pulls its stylesheet from `/console.css`; this inlines it so
//! the result opens from disk with nothing running. Everything else is the same
//! code path the server uses, so what this produces is what gets served.
//!
//! The state below is invented, and deliberately mixed: one service down, one
//! unhealthy, one optional stopped, a failed run and a missed one. A preview
//! where everything is green shows none of the states that matter.

use hajime_console::collect::{Fetched, Reach, ServiceView};
use hajime_console::render;

fn view(
    name: &'static str,
    description: &'static str,
    essential: bool,
    port: Option<u16>,
    mb: u32,
    reach: Reach,
    detail: Option<&str>,
) -> ServiceView {
    ServiceView {
        name,
        description,
        essential,
        port,
        typical_mb: mb,
        reach,
        detail: detail.map(str::to_string),
    }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "console-preview.html".to_string());

    let views = vec![
        view("postgresql", "PostgreSQL 17", true, Some(5432), 500, Reach::Up, Some("port open")),
        view("mysql", "MariaDB, the shop database", true, Some(3306), 400, Reach::Up, Some("port open")),
        view("redis", "queue and cache", true, Some(6379), 60, Reach::Up, Some("port open")),
        view("caddy", "TLS and the public sites", true, Some(80), 50, Reach::Up, None),
        view("cloudflared", "the tunnel", true, None, 50, Reach::Down, None),
        view("hajime_workflow", "the workflow engine", true, Some(5678), 25, Reach::Unhealthy, Some("HTTP 503")),
        view("hajime_wa", "WhatsApp gateway", false, Some(3000), 40, Reach::Down, None),
        view("hajime_wa_bridge", "the Go bridge", false, Some(3001), 60, Reach::Down, None),
        view("hajime_ai", "model gateway", false, Some(11434), 30, Reach::Down, None),
        view("llamacpp", "inference engine", false, Some(11435), 2200, Reach::Down, None),
    ];

    // A history with the shapes worth seeing: a success, a real failure, a
    // scheduled run that was missed while the machine was off, and a shadow
    // run that touched nothing.
    let history = Fetched::ok(serde_json::json!({
        "enabled": true,
        "total": 4,
        "failed": 2,
        "recent": [
            {
                "workflow": "Nightly backup", "trigger": "schedule",
                "duration_ms": 0, "success": false,
                "error": "missed: 6 occurrence(s) of '0 3 * * *' came due while the scheduler was down for 371 minute(s)"
            },
            {
                "workflow": "Post to social", "trigger": "shadow",
                "duration_ms": 3, "success": true
            },
            {
                "workflow": "Health monitor", "trigger": "schedule",
                "duration_ms": 157, "success": false,
                "failed_node": "Notify",
                "error": "request failed: error sending request for url (https://api.example.invalid/send)"
            },
            {
                "workflow": "TOTP Webhook API", "trigger": "In",
                "duration_ms": 78, "success": true
            }
        ]
    }));

    let audit = Fetched::ok(serde_json::json!({
        "calls": [
            { "tool": "fetch_page",     "caller": "model", "effect": "read",     "allowed": true,  "dry_run": false },
            { "tool": "social_post_x",  "caller": "model", "effect": "external", "allowed": true,  "dry_run": true  },
            { "tool": "system_stop",    "caller": "model", "effect": "external", "allowed": false, "dry_run": false },
        ]
    }));

    let environments = vec![
        "hajime-preinstall-20260805-004612".to_string(),
        "hajime-before-upgrade-20260803-221500".to_string(),
    ];

    let headline = hajime_console::collect::headline(&views);
    let page = render::page(
        hajime_console::i18n::Lang::En,
        &headline,
        &views,
        &history,
        &audit,
        Some(environments.as_slice()),
    );

    // Inline the stylesheet so the file opens on its own.
    let css = include_str!("../static/console.css");
    let page = page.replace(
        r#"<link rel="stylesheet" href="/console.css">"#,
        &format!("<style>\n{css}\n</style>"),
    );
    // A preview that reloads every thirty seconds against nothing is a preview
    // that goes blank.
    let page = page.replace(r#"<meta http-equiv="refresh" content="30">"#, "");

    match std::fs::write(&out, &page) {
        Ok(()) => println!("wrote {out} ({} bytes)", page.len()),
        Err(e) => {
            eprintln!("could not write {out}: {e}");
            std::process::exit(1);
        }
    }
}
