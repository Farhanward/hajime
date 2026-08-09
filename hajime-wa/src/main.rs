//! Hajime WhatsApp gateway.
//!
//! Environment:
//!   HAJIME_WA_BIND     listen address, default 127.0.0.1:3000
//!   HAJIME_WA_BRIDGE   whatsmeow bridge URL, default http://127.0.0.1:3001
//!   HAJIME_WA_SECRETS  directory holding `wa_api_key`
//!   HAJIME_WA_HISTORY  send log (JSON Lines)

use hajime_core::{config::Common, history::History, secrets};
use hajime_wa::{api::{self, AppState}, bridge::{Bridge, HttpBridge}};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = match Common::from_env("WA", "127.0.0.1:3000") {
        Ok(c) => c,
        Err(e) => { eprintln!("{e}"); std::process::exit(2); }
    };

    // The API key moves out of the workflow file and into the secret store.
    let api_key = match &cfg.secrets_dir {
        Some(dir) => match secrets::Store::from_dir(dir) {
            Ok(store) => store.try_get("wa_api_key").cloned().map(Arc::new),
            Err(e) => { eprintln!("{e}"); std::process::exit(2); }
        },
        None => None,
    };

    if let Err(e) = cfg.guard_exposure(api_key.is_some(), "wa_api_key in HAJIME_WA_SECRETS") {
        eprintln!("{e}");
        std::process::exit(2);
    }

    let bridge_url = std::env::var("HAJIME_WA_BRIDGE")
        .unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let bridge = Arc::new(HttpBridge::new(&bridge_url));

    let history = Arc::new(match &cfg.history_path {
        Some(p) => History::at(p),
        None => History::disabled(),
    });

    tracing::info!(bridge = %bridge_url, authenticated = api_key.is_some(), "starting");
    if api_key.is_none() {
        tracing::warn!("no wa_api_key configured: bound to loopback only");
    }
    if !history.is_enabled() {
        tracing::warn!("HAJIME_WA_HISTORY unset: sends will not be recorded");
    }

    // Report the bridge once at startup rather than letting the first send
    // discover it.
    match bridge.session("default").await {
        Ok(s) if s.is_working() => tracing::info!(status = %s.status, "bridge connected"),
        Ok(s) => tracing::warn!(status = %s.status, "bridge reachable but not paired: sends will be refused"),
        Err(e) => tracing::error!(error = %e, "bridge unreachable: sends will be refused"),
    }

    let state = AppState { bridge, history, api_key, started: chrono::Utc::now() };

    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(e) => { eprintln!("could not bind {}: {e}", cfg.bind); std::process::exit(1); }
    };
    tracing::info!("listening on http://{}", cfg.bind);

    if let Err(e) = axum::serve(listener, api::router(state)).await {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}
