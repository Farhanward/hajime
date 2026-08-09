//! Runs the exported Health Monitor workflow and prints what each node did,
//! so the result can be held next to an n8n execution of the same file.
//!
//!   cargo run -p hajime-workflow --example run_health_monitor

use hajime_workflow::engine::Engine;
use hajime_workflow::model::Workflow;
use hajime_workflow::nodes::Registry;

#[tokio::main]
async fn main() {
    let raw = include_str!("../tests/fixtures/health_monitor.json");
    let wf: Workflow = serde_json::from_str(raw).expect("export should load");

    println!("workflow : {}", wf.name);
    println!("nodes    : {}", wf.nodes.len());
    println!();

    let engine = Engine::new(Registry::with_builtins());
    let result = engine
        .run(&wf, Some("Manual Execute Trigger"), vec![])
        .await
        .expect("run should start");

    println!("{:<24} {:<24} {:>6} {:>12}", "NODE", "KIND", "ITEMS", "TIME");
    println!("{}", "-".repeat(70));
    for run in &result.runs {
        let time = if run.duration_us >= 1000 {
            format!("{:.1} ms", run.duration_us as f64 / 1000.0)
        } else {
            format!("{} us", run.duration_us)
        };
        println!(
            "{:<24} {:<24} {:>6} {:>12}",
            run.node, run.kind, run.items_out, time
        );
        if let Some(err) = &run.error {
            println!("    error: {err}");
        }
    }

    println!();
    println!("success  : {}", result.success);
    println!("total    : {:.1} ms", result.duration_us as f64 / 1000.0);

    if let Some(items) = result.outputs.get("Fetch Health Page") {
        println!();
        // The item is the response body, as n8n emits it. There is no status
        // field unless the node asks for the full response.
        println!("responses (body only, matching n8n):");
        for (i, item) in items.iter().enumerate() {
            let rendered = serde_json::to_string(&item.json).unwrap_or_default();
            let shape = if item.json.get("data").is_some() {
                "text"
            } else {
                "json"
            };
            println!("  {} → {:<5} {} bytes", i + 1, shape, rendered.len());
        }
    }

    if let Some(items) = result.outputs.get("Summarize Health") {
        println!();
        println!("summary  : {}", serde_json::to_string(&items[0].json).unwrap());
    }
}
