//! Runs a workflow exported from a live n8n instance, unmodified apart from
//! the domain and workflow name, which are replaced with placeholders here.
//!
//! The fixture is a real export (`🚨 Example Co — Hourly Health Monitor`),
//! taken from a production backup. It exercises four node kinds at once and,
//! more usefully, it catches the two things unit tests could not: parameters
//! that are expressions evaluated per item, and nodes that must run once per
//! item rather than once per list.
//!
//! The network-touching case is marked `#[ignore]`: run it with
//! `cargo test -p hajime-workflow -- --ignored` when the sites should be up.

use hajime_workflow::engine::Engine;
use hajime_workflow::model::Workflow;
use hajime_workflow::nodes::Registry;

const FIXTURE: &str = include_str!("fixtures/health_monitor.json");

fn workflow() -> Workflow {
    serde_json::from_str(FIXTURE).expect("the n8n export should load unchanged")
}

#[test]
fn the_export_loads_without_conversion() {
    let wf = workflow();
    assert_eq!(wf.name, "🚨 Example Co — Hourly Health Monitor");
    assert!(wf.active);
    assert_eq!(wf.nodes.len(), 5);

    let kinds: Vec<&str> = wf.nodes.iter().map(|n| n.kind()).collect();
    for expected in [
        "executeWorkflowTrigger",
        "scheduleTrigger",
        "code",
        "httpRequest",
    ] {
        assert!(kinds.contains(&expected), "missing node kind {expected}");
    }
}

#[test]
fn both_triggers_are_recognised() {
    let wf = workflow();
    let mut names: Vec<&str> = wf.triggers().iter().map(|n| n.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["Every Hour", "Manual Execute Trigger"]);
}

#[tokio::test]
async fn the_build_step_produces_one_item_per_path() {
    // Stops before the HTTP node so this stays offline: the Code node is the
    // part that decides how many requests the next node will make.
    let mut wf = workflow();
    wf.nodes
        .iter_mut()
        .find(|n| n.name == "Fetch Health Page")
        .unwrap()
        .disabled = true;

    let engine = Engine::new(Registry::with_builtins());
    let result = engine
        .run(&wf, Some("Manual Execute Trigger"), vec![])
        .await
        .expect("the workflow should run");

    assert!(result.success, "run failed: {:?}", result.runs);

    let built = result
        .outputs
        .get("Build Health Checks")
        .expect("the code node should have produced output");
    assert_eq!(built.len(), 5, "one item per health-check path");

    let paths: Vec<&str> = built
        .iter()
        .map(|i| i.json["path"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        paths,
        vec!["/", "/robots.txt", "/llms.txt", "/sitemap.xml", "/index.md"]
    );

    // The Code node calls `new Date().toISOString()`, so the sandbox must have
    // a working clock rather than a stub.
    for item in built {
        let stamp = item.json["started_at"].as_str().unwrap_or_default();
        assert!(stamp.contains('T') && stamp.ends_with('Z'), "bad stamp: {stamp}");
        assert!(item.json["url"].as_str().unwrap().starts_with("https://"));
    }
}

#[tokio::test]
async fn the_summary_step_counts_what_it_received() {
    let mut wf = workflow();
    wf.nodes
        .iter_mut()
        .find(|n| n.name == "Fetch Health Page")
        .unwrap()
        .disabled = true;

    let engine = Engine::new(Registry::with_builtins());
    let result = engine
        .run(&wf, Some("Manual Execute Trigger"), vec![])
        .await
        .unwrap();

    let summary = result.outputs.get("Summarize Health").unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].json["ok"], true);
    assert_eq!(summary[0].json["checks"], 5);
    assert_eq!(
        summary[0].json["checked_paths"].as_array().unwrap().len(),
        5
    );
}

#[tokio::test]
#[ignore = "makes real requests to example.com"]
async fn full_run_against_the_live_site() {
    let engine = Engine::new(Registry::with_builtins());
    let result = engine
        .run(&workflow(), Some("Manual Execute Trigger"), vec![])
        .await
        .expect("the workflow should run");

    assert!(result.success, "run failed: {:?}", result.runs);

    // Five paths in, five responses out: the HTTP node runs per item.
    let fetched = result.outputs.get("Fetch Health Page").unwrap();
    assert_eq!(fetched.len(), 5, "expected one response per path");

    // The item is the response body, matching n8n. Stored execution 2017
    // contains no `statusCode` key, so wrapping here would diverge.
    for item in fetched {
        assert!(
            item.json.get("statusCode").is_none(),
            "the default shape must not wrap the body"
        );
        assert!(!item.json.is_null(), "a body should have been returned");
    }

    let summary = result.outputs.get("Summarize Health").unwrap();
    assert_eq!(summary[0].json["checks"], 5);

    // n8n produces the same thing: httpRequest replaces the item, so the
    // original `path` is gone by the time the summary runs. Matching the
    // quirk matters more than improving on it during a migration.
    let paths = summary[0].json["checked_paths"].as_array().unwrap();
    assert!(
        paths.iter().all(|p| p == "unknown"),
        "n8n records 'unknown' here; diverging would change the output"
    );
}
