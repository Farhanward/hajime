//! Hajime console.
//!
//! Environment:
//!   HAJIME_CONSOLE_BIND      listen address, default 127.0.0.1:8088
//!   HAJIME_CONSOLE_SECRETS   directory holding `console_token`, and the
//!                            tokens the console presents to other services
//!   HAJIME_WORKFLOW_URL      default http://127.0.0.1:5678
//!   HAJIME_AI_URL            default http://127.0.0.1:11434

use axum::{
    extract::{RawQuery, State},
    http::HeaderMap,
    response::Html,
    routing::get,
    Router,
};
use hajime_console::{collect::Collector, i18n::Lang, render};
use hajime_core::{config::Common, secrets, Auth};
use hajime_sys::snapshot;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    collector: Arc<Collector>,
    workflow_url: String,
    ai_url: String,
    workflow_token: Option<Arc<String>>,
    ai_token: Option<Arc<String>>,
}

async fn index(
    State(s): State<AppState>,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Html<String> {
    // The language is a property of whoever is reading, not of the page: a
    // chosen `?lang=` first, the browser's list second, English if neither says
    // anything. Same rule as the login class, one layer up.
    let lang = Lang::from_request(
        query.as_deref(),
        headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    );

    let views = s.collector.services().await;
    let headline = hajime_console::collect::headline(&views);

    let history = s
        .collector
        .workflow_history(&s.workflow_url, s.workflow_token.as_deref().map(String::as_str))
        .await;
    let audit = s
        .collector
        .tool_audit(&s.ai_url, s.ai_token.as_deref().map(String::as_str))
        .await;

    // Absent bectl means no rollback exists, which the page states rather than
    // rendering an empty list that reads as "none taken yet".
    let environments = snapshot::list_boot_environments().ok();

    Html(render::page(
        lang,
        &headline,
        &views,
        &history,
        &audit,
        environments.as_deref(),
    ))
}

/// The stylesheet, palette first.
///
/// Two files, one response. The palette is generated from palette.toml by
/// hajime-brand, so the console cannot drift from the desktop and the kernel
/// console about what any colour is; console.css is the part that is only this
/// page's business. Concatenating them here rather than using an `@import`
/// saves the browser a second request on a page whose whole point is to load
/// when the machine is unwell.
async fn stylesheet() -> axum::response::Response {
    use axum::http::header;
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        concat!(
            include_str!("../../hajime-brand/out/palette.css"),
            include_str!("../static/console.css"),
        ),
    )
        .into_response()
}

/// The mascot's head, for the tab's icon.
async fn mark() -> axum::response::Response {
    use axum::http::header;
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        include_str!("../../hajime-brand/out/mark.svg"),
    )
        .into_response()
}

use axum::response::IntoResponse;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = match Common::from_env("CONSOLE", "127.0.0.1:8088") {
        Ok(c) => c,
        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
    };

    let store = match &cfg.secrets_dir {
        Some(dir) => match secrets::Store::from_dir(dir) {
            Ok(s) => s,
            Err(e) => { eprintln!("{e}"); std::process::exit(2); }
        },
        None => secrets::Store::empty(),
    };

    let auth = Auth::new(store.try_get("console_token").map(|s| s.expose().to_string()));
    // The console shows the audit log and every service's state. Exposing that
    // without a token would hand an attacker a map of the system.
    if let Err(e) = cfg.guard_exposure(auth.is_enabled(), "console_token in HAJIME_CONSOLE_SECRETS") {
        eprintln!("{e}");
        std::process::exit(2);
    }

    let state = AppState {
        collector: Arc::new(Collector::default()),
        workflow_url: std::env::var("HAJIME_WORKFLOW_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:5678".into()),
        ai_url: std::env::var("HAJIME_AI_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".into()),
        workflow_token: store.try_get("workflow_token").map(|s| Arc::new(s.expose().to_string())),
        ai_token: store.try_get("ai_token").map(|s| Arc::new(s.expose().to_string())),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/console.css", get(stylesheet))
        .route("/mark.svg", get(mark))
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            hajime_core::auth::require_token,
        ))
        .with_state(state);

    tracing::info!(authenticated = auth.is_enabled(), "starting");
    if !auth.is_enabled() {
        tracing::warn!("no console_token: bound to loopback only");
    }

    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(e) => { eprintln!("could not bind {}: {e}", cfg.bind); std::process::exit(1); }
    };
    tracing::info!("listening on http://{}", cfg.bind);

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}
