//! Proves the store resolves every schedule shape the real n8n export uses,
//! not just the `cronExpression` form the unit tests happened to cover.
//!
//! `hajime-workflow --validate` run against a real production export (17
//! workflows, 5 active) passed clean: every node kind in the file has an
//! executor and no schedule was rejected. But three of the four active
//! schedules use `cronExpression` directly, and `triggers.rs` only
//! unit-tested the `minutes` and `days` friendly forms of the interval
//! field -- never `hours`. The one active workflow that uses it, "Node B -
//! SEO and Content Optimization (RAG)", was validated only by the flag's
//! exit code, not by a test that would fail loudly if the conversion broke.
//!
//! The fixture is a trimmed, shape-faithful copy of that workflow: the same
//! two trigger kinds fanning into the same node, `rssFeedRead` feeding
//! `code` feeding `httpRequest`, `hoursInterval: 12` on the schedule.

use hajime_workflow::nodes::Registry;
use hajime_workflow::store::Store;

const FIXTURE: &str = include_str!("fixtures/seo_schedule.json");

fn store() -> Store {
    // Wrapped in an array: this is the shape `n8n export:workflow --all`
    // produces and the shape `Store::from_export` is actually fed in
    // production, not a bare object.
    Store::from_export(&format!("[{FIXTURE}]")).expect("the fixture should load unchanged")
}

#[test]
fn the_hours_interval_form_resolves_to_a_cron_expression() {
    // hoursInterval: 12 must become "0 */12 * * *", the same conversion
    // `ScheduleTrigger::expressions` performs for the "minutes" and "days"
    // forms that already had unit coverage.
    let schedules = store().schedules();
    let seo = schedules
        .iter()
        .find(|s| s.node == "SEO Schedule")
        .expect("the SEO Schedule node should produce a schedule");
    assert_eq!(seo.expression, "0 */12 * * *");
}

#[test]
fn the_hours_interval_schedule_is_not_flagged_as_broken() {
    // broken_schedules() is what --validate exits non-zero on. A regression
    // in the "hours" branch would surface there, silently, on the next
    // restore rehearsal rather than in a test run.
    let broken = store().broken_schedules();
    assert!(broken.is_empty(), "unexpected broken schedule(s): {broken:?}");
}

#[test]
fn every_node_kind_in_the_seo_workflow_has_an_executor() {
    // executeWorkflowTrigger, scheduleTrigger, rssFeedRead, code and
    // httpRequest: the five kinds the real workflow uses. A gap here would
    // show up in --validate as "node kinds with no executor" but nothing
    // in the test suite asserted it stays empty for this specific shape.
    let registry = Registry::with_builtins();
    let known: Vec<&str> = registry.kinds();
    let summaries = store().summaries(&known);
    let seo = summaries
        .iter()
        .find(|s| s.id == "seo-node-b")
        .expect("the fixture workflow should be in the store");
    assert!(
        seo.unsupported.is_empty(),
        "unexpected unsupported node kind(s): {:?}",
        seo.unsupported
    );
}

#[test]
fn both_trigger_kinds_are_recognised_as_alternative_entry_points() {
    let schedules = store().schedules();
    // Only the schedule trigger produces a cron entry; the execute-workflow
    // trigger is a second, alternative way into the same downstream node and
    // must not appear here.
    assert_eq!(schedules.len(), 1);
}
