//! HTTP surface: webhook routing, status and manual runs.
//!
//! Webhook paths are matched against the routing table built from the loaded
//! workflows, so a request reaches the same workflow n8n would have run. The
//! reply follows the trigger's `responseMode`.

use crate::auth::{self, Auth};
use crate::engine::Engine;
use crate::engine::record_of;
use crate::history::History;
use crate::model::Item;
use crate::nodes::respond_to_webhook::{take_response, without_marker};
use crate::nodes::triggers::{ResponseMode, Webhook};
use crate::store::Store;
use axum::{
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Json},
    routing::{any, get},
    Router,
};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub engine: Arc<Engine>,
    /// The same registry, wired to hold back anything that changes the world.
    /// Kept alongside the live engine rather than swapped in by a restart, so a
    /// workflow can be rehearsed on a running system without taking the real
    /// schedules offline to do it.
    pub shadow: Arc<Engine>,
    pub history: Arc<History>,
    pub auth: Auth,
    pub known_kinds: Vec<String>,
    pub started: chrono::DateTime<chrono::Utc>,
}

pub fn router(state: AppState) -> Router {
    // Control endpoints need a token; webhooks stay open because that is the
    // public surface and n8n treats it the same way.
    let control = Router::new()
        .route("/status", get(status))
        .route("/api/workflows", get(list))
        .route("/api/workflows/:id/run", axum::routing::post(run_manually))
        .route("/api/history", get(history))
        .layer(axum::middleware::from_fn_with_state(
            state.auth.clone(),
            auth::require_token,
        ))
        .with_state(state.clone());

    Router::new()
        .route("/health", get(health))
        .route("/webhook/*path", any(webhook))
        .route("/webhook-test/*path", any(webhook))
        .with_state(state)
        .merge(control)
}

async fn history(State(state): State<AppState>) -> impl IntoResponse {
    let (total, failed) = state.history.stats();
    Json(serde_json::json!({
        "enabled": state.history.is_enabled(),
        "total": total,
        "failed": failed,
        "recent": state.history.recent(50),
    }))
}

/// Liveness only. It reports what this process actually does, with no claim
/// about capabilities that are not wired up.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "engine": "hajime-workflow",
        "workflows_loaded": state.store.len(),
        "uptime_seconds": (chrono::Utc::now() - state.started).num_seconds(),
    }))
}

/// Everything an operator needs to see whether a migration is safe yet:
/// which routes are live, which schedules resolved, and which node kinds in
/// the loaded workflows still have no executor.
async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let known: Vec<&str> = state.known_kinds.iter().map(String::as_str).collect();
    let summaries = state.store.summaries(&known);
    let unsupported: Vec<&str> = {
        let mut all: Vec<&str> = summaries
            .iter()
            .flat_map(|s| s.unsupported.iter().map(String::as_str))
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    };

    let broken: Vec<serde_json::Value> = state
        .store
        .broken_schedules()
        .into_iter()
        .map(|(wf, node, reason)| serde_json::json!({
            "workflow": wf, "node": node, "reason": reason
        }))
        .collect();

    Json(serde_json::json!({
        "workflows": summaries.len(),
        "active": summaries.iter().filter(|s| s.active).count(),
        "routes": state.store.routes().len(),
        "schedules": state.store.schedules().len(),
        "broken_schedules": broken,
        "node_kinds_without_executor": unsupported,
        "supported_node_kinds": state.known_kinds,
    }))
}

async fn list(State(state): State<AppState>) -> impl IntoResponse {
    let known: Vec<&str> = state.known_kinds.iter().map(String::as_str).collect();
    Json(state.store.summaries(&known))
}

async fn run_manually(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    // `?shadow=1` walks the graph without letting any node change anything.
    // The imported workflows post to WhatsApp and to four social accounts, so
    // the first run of one after a migration should not be the real one.
    let dry = matches!(params.get("shadow").map(String::as_str), Some("1" | "true" | "yes"));
    let Some(workflow) = state.store.get(&id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("no workflow '{id}'") })),
        );
    };

    let payload = body
        .map(|Json(v)| vec![Item::from_json(v)])
        .unwrap_or_default();

    let started_at = chrono::Utc::now();
    let engine = if dry { &state.shadow } else { &state.engine };
    // Recorded under its own trigger name. A rehearsal that appears in the
    // history as an ordinary run would later be read as evidence the workflow
    // really ran.
    let trigger = if dry { "shadow" } else { "manual" };
    match engine.run(&workflow, None, payload).await {
        Ok(result) => {
            state.history.append(&record_of(&id, trigger, started_at, &result));
            let code = if result.success {
                StatusCode::OK
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            (code, Json(serde_json::to_value(&result).unwrap_or_default()))
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn webhook(
    State(state): State<AppState>,
    method: Method,
    Path(path): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let wanted = path.trim_start_matches('/');
    let route = state
        .store
        .routes()
        .into_iter()
        .find(|r| r.path == wanted && r.method == method.as_str());

    let Some(route) = route else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no active webhook for {} /{}", method, wanted)
            })),
        );
    };

    let Some(workflow) = state.store.get(&route.workflow_id) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "route points at a missing workflow" })),
        );
    };

    // n8n hands the trigger one item describing the request.
    let payload = vec![Item::from_json(serde_json::json!({
        "headers": {},
        "params": {},
        "query": query,
        "body": body.map(|Json(v)| v).unwrap_or(serde_json::Value::Null),
    }))];

    let mode = workflow
        .node(&route.node)
        .map(Webhook::response_mode)
        .unwrap_or(ResponseMode::LastNode);

    let started_at = chrono::Utc::now();
    let outcome = state.engine.run(&workflow, Some(&route.node), payload).await;
    if let Ok(result) = &outcome {
        state
            .history
            .append(&record_of(&route.workflow_id, &route.node, started_at, result));
    }
    match outcome {
        Ok(result) if result.success => {
            let body = match mode {
                ResponseMode::ResponseNode => result
                    .outputs
                    .values()
                    .find_map(|items| take_response(items))
                    .unwrap_or(serde_json::Value::Null),
                ResponseMode::LastNode => result
                    .last_items()
                    .and_then(|items| items.first())
                    .map(|i| without_marker(i.json.clone()))
                    .unwrap_or(serde_json::Value::Null),
                ResponseMode::Immediately => {
                    serde_json::json!({ "message": "workflow started" })
                }
            };
            (StatusCode::OK, Json(body))
        }
        Ok(result) => {
            let failed = result.runs.iter().find(|r| r.error.is_some());
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "workflow failed",
                    "node": failed.map(|r| r.node.clone()),
                    "detail": failed.and_then(|r| r.error.clone()),
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}
