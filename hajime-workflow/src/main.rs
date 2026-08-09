//! Hajime workflow service.
//!
//! Loads workflows from an n8n export, serves their webhooks, and runs their
//! schedules. Binds to loopback by default: the previous placeholder listened
//! on every interface with no authentication, and these workflows reach live
//! systems.
//!
//! Environment:
//!   HAJIME_WORKFLOWS  path to an `n8n export:workflow --all` file (required)
//!   HAJIME_BIND       listen address, default 127.0.0.1:5678
//!   HAJIME_TZ         schedule timezone, default Asia/Riyadh
//!   HAJIME_TOKEN      bearer token for the control endpoints
//!   HAJIME_HISTORY    path to the execution log (JSON Lines)
//!   HAJIME_ALLOW_COMMAND / HAJIME_ALLOW_SSH / HAJIME_FILE_ROOTS
//!                     host-touching capabilities, all off by default

use hajime_workflow::{
    auth::Auth,
    engine::Engine,
    history::History,
    nodes::{ssh::SshTarget, Registry},
    policy::Policy,
    scheduler::{self, Scheduler},
    server::{self, AppState},
    store::Store,
};
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // `--validate` loads the workflows, reports what the engine makes of them
    // and exits without binding a port. The restore uses it to check that the
    // file it just copied parses with the engine that will run it: a workflow
    // that is copied but unparseable is a workflow that was never restored.
    let validate_only = std::env::args().any(|a| a == "--validate");

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        )
        .init();

    let path = match std::env::var("HAJIME_WORKFLOWS") {
        Ok(p) => p,
        Err(_) => {
            eprintln!("HAJIME_WORKFLOWS is not set.");
            eprintln!("Point it at a file produced by `n8n export:workflow --all`.");
            std::process::exit(2);
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("could not read {path}: {e}");
            std::process::exit(2);
        }
    };

    let store = match Store::from_export(&raw) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            eprintln!("could not load {path}: {e}");
            std::process::exit(2);
        }
    };

    let policy = Policy::from_env();
    let ssh_target = SshTarget::from_env();
    let registry = Registry::with_policy(policy.clone(), ssh_target);
    let known_kinds: Vec<String> = registry.kinds().iter().map(|k| k.to_string()).collect();
    // Cloned, not rebuilt: the executors sit behind Arc, so the shadow engine
    // further down shares this registry's connection pool instead of opening a
    // second one that would sit idle most of the time.
    let engine = Arc::new(Engine::new(registry.clone()));

    if validate_only {
        let known: Vec<&str> = known_kinds.iter().map(String::as_str).collect();
        let summaries = store.summaries(&known);
        let active = summaries.iter().filter(|s| s.active).count();

        println!("{path}");
        println!("  {} workflow(s), {active} active", store.len());
        println!("  {} webhook route(s)", store.routes().len());
        println!("  {} schedule(s)", store.schedules().len());

        // An unrecognised node kind is not a parse error, so it would pass a
        // check that only asked "did this deserialise". It matters anyway: such
        // a node passes its input through, and the workflow around it still
        // reports success.
        let mut unknown: Vec<String> = summaries
            .iter()
            .filter(|s| s.active)
            .flat_map(|s| s.unsupported.clone())
            .collect();
        unknown.sort_unstable();
        unknown.dedup();
        if !unknown.is_empty() {
            println!("  node kinds with no executor: {}", unknown.join(", "));
            println!("  those nodes pass their input through untouched");
        }

        let broken = store.broken_schedules();
        for (workflow, node, reason) in &broken {
            println!("  schedule ignored: {workflow} / {node}: {reason}");
        }

        // Exit non-zero only for a schedule that will never fire. That is a
        // workflow silently doing nothing, which is the failure this flag
        // exists to catch. Unknown node kinds are reported but not fatal:
        // several are deliberate omissions.
        if broken.is_empty() {
            println!("ok");
            std::process::exit(0);
        }
        eprintln!("{} schedule(s) will never fire", broken.len());
        std::process::exit(1);
    }

    let auth = Auth::from_env();
    let history = Arc::new(History::from_env());
    let tz = scheduler::timezone(std::env::var("HAJIME_TZ").ok().as_deref());
    let bind: SocketAddr = std::env::var("HAJIME_BIND")
        .unwrap_or_else(|_| "127.0.0.1:5678".to_string())
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("HAJIME_BIND is not a valid address: {e}");
            std::process::exit(2);
        });

    // Refuse to listen beyond loopback without a token. The control endpoints
    // can start any workflow, and these workflows reach live systems.
    if !bind.ip().is_loopback() && !auth.is_enabled() {
        eprintln!(
            "refusing to bind {bind} without HAJIME_TOKEN: the control endpoints can \
             start any workflow. Set a token, or bind to 127.0.0.1."
        );
        std::process::exit(2);
    }

    let routes = store.routes();
    let schedules = store.schedules();

    tracing::info!(
        workflows = store.len(),
        routes = routes.len(),
        schedules = schedules.len(),
        timezone = %tz,
        "loaded {path}"
    );

    // Surface partial ports at startup. A node kind with no executor passes
    // data through, so a workflow can look successful while doing nothing.
    let unsupported: Vec<String> = {
        let known: Vec<&str> = known_kinds.iter().map(String::as_str).collect();
        let mut all: Vec<String> = store
            .summaries(&known)
            .into_iter()
            .filter(|s| s.active)
            .flat_map(|s| s.unsupported)
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    };
    if policy.allow_command {
        tracing::warn!(
            "executeCommand is ENABLED: any workflow reaching that node can run \
             shell commands on this host"
        );
    }
    if policy.allow_ssh {
        tracing::warn!("ssh is ENABLED: workflows can run commands on the configured host");
    }
    if !policy.file_roots.is_empty() {
        tracing::info!(roots = ?policy.file_roots, "file access permitted under");
    }
    if !history.is_enabled() {
        tracing::warn!(
            "HAJIME_HISTORY is unset: runs will not be recorded, so a failed \
             scheduled job leaves no trace"
        );
    }

    if !unsupported.is_empty() {
        tracing::warn!(
            kinds = ?unsupported,
            "active workflows use node kinds with no executor; those nodes pass \
             their input through untouched"
        );
    }
    for (workflow, node, reason) in store.broken_schedules() {
        tracing::warn!(%workflow, %node, %reason, "schedule ignored");
    }

    // At-most-once. Two copies of this service means every scheduled job runs
    // twice, and nothing in either process notices: both log success and both
    // write history. The duplicate only shows up as a doubled post or a
    // doubled charge somewhere downstream, hours later.
    //
    // Held for the lifetime of the process. The binding keeps it alive; naming
    // it `_` would drop it here and release the lock immediately.
    let lock_path = std::env::var("HAJIME_LOCK")
        .unwrap_or_else(|_| "/vault/hajime/workflow.lock".to_string());
    let _lock = match hajime_workflow::lock::acquire(&lock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            eprintln!(
                "Another workflow service is already running. Stop it first, \
                 or set HAJIME_LOCK to a different path if you genuinely want \
                 a second instance with its own schedules."
            );
            std::process::exit(3);
        }
    };

    let mut sched = Scheduler::new(
        Arc::clone(&store),
        Arc::clone(&engine),
        Arc::clone(&history),
        tz,
    )
    .with_catchup(scheduler::Catchup::from_env());

    // The tick file sits beside the history, so both move together when the
    // data directory does.
    if let Ok(state) = std::env::var("HAJIME_SCHEDULER_STATE") {
        sched = sched.with_state_file(state);
    } else if let Ok(h) = std::env::var("HAJIME_HISTORY") {
        let mut p = std::path::PathBuf::from(h);
        p.set_file_name("scheduler.tick");
        sched = sched.with_state_file(p);
    } else {
        tracing::warn!(
            "no HAJIME_HISTORY or HAJIME_SCHEDULER_STATE: the scheduler cannot \
             tell what it missed while it was down"
        );
    }

    tokio::spawn(sched.run_forever());

    // The same executors, wired to hold back anything that changes the world.
    let shadow = Arc::new(Engine::shadow(registry));

    let state = AppState {
        store,
        engine,
        shadow,
        history,
        auth,
        known_kinds,
        started: chrono::Utc::now(),
    };

    let listener = match tokio::net::TcpListener::bind(bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!("listening on http://{bind}");

    if let Err(e) = axum::serve(listener, server::router(state)).await {
        tracing::error!(error = %e, "server stopped");
        std::process::exit(1);
    }
}
