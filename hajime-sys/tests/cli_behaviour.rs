//! Behaviour of `hajimectl` that would be dangerous to get wrong.
//!
//! These run the real binary. They avoid anything that would change the
//! machine: on a development box there is no `service(8)` and no ZFS, so the
//! destructive paths report their absence instead of acting, which is itself
//! the behaviour under test.

use std::process::Command;

fn hajimectl(args: &[&str]) -> (String, String, i32) {
    let exe = env!("CARGO_BIN_EXE_hajimectl");
    let out = Command::new(exe).args(args).output().expect("binary should run");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn no_arguments_prints_usage_and_fails() {
    let (_, err, code) = hajimectl(&[]);
    assert_eq!(code, 2);
    assert!(err.contains("usage"));
}

#[test]
fn an_unknown_service_is_refused_before_anything_happens() {
    // A typo must never become "operate on everything". The exit code
    // distinguishes it from an operational failure.
    let (_, err, code) = hajimectl(&["start", "postgres"]); // not `postgresql`
    assert_eq!(code, 2, "a bad name is a usage error, not a failure");
    assert!(err.contains("no service named 'postgres'"));
    assert!(err.contains("postgresql"), "it should list what is valid");
}

#[test]
fn a_known_service_name_is_accepted() {
    let (_, err, _) = hajimectl(&["stop", "hajime_ai"]);
    assert!(!err.contains("no service named"));
}

#[test]
fn snapshot_without_a_reason_is_refused() {
    // An unnamed snapshot is one nobody can identify later.
    let (_, err, code) = hajimectl(&["snapshot"]);
    assert_eq!(code, 2);
    assert!(err.contains("needs a reason"));
}

#[test]
fn snapshot_without_zfs_explains_and_changes_nothing() {
    let (_, err, code) = hajimectl(&["snapshot", "before-upgrade"]);
    assert_ne!(code, 0);
    assert!(err.contains("Nothing was changed"), "got: {err}");
}

#[test]
fn restart_without_a_name_refuses_rather_than_restarting_everything() {
    // Restarting the whole system because an argument was forgotten would be
    // an outage caused by a typo.
    let (_, err, code) = hajimectl(&["restart"]);
    assert_eq!(code, 2);
    assert!(err.contains("needs a service name"));
}

#[test]
fn check_reports_not_ready_when_essential_services_are_down() {
    let (out, _, code) = hajimectl(&["check"]);
    assert_ne!(code, 0, "nothing is running here, so it must not claim ready");
    assert!(out.contains("NOT READY"));
    // Failures must be readable first.
    let fail_at = out.find("FAIL").expect("a failure line");
    let warn_at = out.find("warn").expect("a warning line");
    assert!(fail_at < warn_at);
}

#[test]
fn check_separates_essential_failures_from_optional_warnings() {
    let (out, _, _) = hajimectl(&["check"]);
    // An essential service that is down is a failure.
    assert!(out.contains("FAIL  postgresql"), "got:\n{out}");
    // An optional one is only a warning: saving mode stops these deliberately.
    assert!(out.contains("warn  llamacpp"), "got:\n{out}");
}

#[test]
fn status_reports_failure_when_essential_services_are_down() {
    let (out, _, code) = hajimectl(&["status"]);
    assert_ne!(code, 0);
    assert!(out.contains("essential service(s) down"));
}

#[test]
fn an_unrecognised_command_prints_usage() {
    let (_, err, code) = hajimectl(&["frobnicate"]);
    assert_eq!(code, 2);
    assert!(err.contains("usage"));
}
