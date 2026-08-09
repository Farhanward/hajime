//! Hajime model gateway.
//!
//! This binary is the composition root: it is the only place that knows which
//! tool providers exist. The library stays unaware of them, so adding a
//! capability never means editing the gateway.
//!
//! Environment:
//!   HAJIME_AI_BIND      listen address, default 127.0.0.1:11434
//!   HAJIME_AI_BACKEND   inference server URL, default http://127.0.0.1:11435
//!   HAJIME_AI_SECRETS   secret directory: `ai_token` plus any platform tokens
//!   HAJIME_AI_LIVE      1 to let tools with external effects actually run
//!   HAJIME_AI_HISTORY   path to the tool audit log (JSON Lines)

use hajime_ai::{
    api::{self, AppState},
    backend::{Backend, HttpBackend},
};
use hajime_core::{
    config::{self, Common},
    secrets,
    tools::{Budget, Gateway},
    Auth,
};
use hajime_fetch::Fetch;
use hajime_social::Social;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = match Common::from_env("AI", "127.0.0.1:11434") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // One secret store for the service token and every platform credential.
    let store = match &cfg.secrets_dir {
        Some(dir) => match secrets::Store::from_dir(dir) {
            Ok(store) => store,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        None => secrets::Store::empty(),
    };

    let auth = Auth::new(store.try_get("ai_token").map(|s| s.expose().to_string()));
    if let Err(e) = cfg.guard_exposure(auth.is_enabled(), "ai_token in HAJIME_AI_SECRETS") {
        eprintln!("{e}");
        std::process::exit(2);
    }

    let backend_url = std::env::var("HAJIME_AI_BACKEND")
        .unwrap_or_else(|_| "http://127.0.0.1:11435".into());
    let backend = Arc::new(HttpBackend::new(&backend_url, 300));

    // Dry run is the default. A new deployment should describe what it would
    // send before it is trusted to send it.
    let dry_run = !config::flag("HAJIME_AI_LIVE");
    let mut gateway = Gateway::new(Budget::default(), dry_run);

    // Register the providers. Each reads its own credentials from the store and
    // offers only the tools it can actually perform.
    let social = Social::from_secrets(&store);
    let social_tools = social.configured().len();
    gateway.add(Box::new(social));

    // Fetching needs no credential, so it is always available. It is a read,
    // which means it still works while the gateway is in dry-run mode: a
    // rehearsal should be able to research even when it cannot publish.
    gateway.add(Box::new(Fetch::default()));

    let gateway = Arc::new(gateway);
    let catalogue = gateway.catalogue().await;

    tracing::info!(
        backend = %backend_url,
        dry_run,
        authenticated = auth.is_enabled(),
        secrets = store.len(),
        tools = catalogue.len(),
        "starting"
    );

    if catalogue.is_empty() {
        tracing::warn!(
            "no tools are available: the model can answer but cannot act. \
             Check that HAJIME_AI_SECRETS holds the platform credentials"
        );
    } else {
        for spec in &catalogue {
            tracing::info!(tool = %spec.name, effect = ?spec.effect, "tool available");
        }
    }
    if social_tools == 0 && !store.is_empty() {
        tracing::warn!("secrets are present but no publishing platform is fully configured");
    }

    if dry_run {
        tracing::warn!(
            "DRY RUN: tools with external effects will be described, not executed. \
             Set HAJIME_AI_LIVE=1 once you have watched the audit log and are ready"
        );
    } else {
        tracing::warn!(
            "LIVE: tools with external effects will really run. Posts and messages \
             sent from here cannot be recalled"
        );
    }

    match backend.models().await {
        Ok(models) if models.is_empty() => {
            tracing::warn!("backend reachable but has no models loaded")
        }
        Ok(models) => tracing::info!(?models, "backend reachable"),
        Err(e) => tracing::error!(error = %e, "backend unreachable: chat requests will fail"),
    }

    let state = AppState { backend, gateway, auth, started: chrono::Utc::now() };

    let listener = match tokio::net::TcpListener::bind(cfg.bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind {}: {e}", cfg.bind);
            std::process::exit(1);
        }
    };
    tracing::info!("listening on http://{}", cfg.bind);

    if let Err(e) = axum::serve(listener, api::router(state)).await {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}
