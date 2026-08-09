//! `hajimectl` — one command for the whole system.
//!
//! Without it you would manage nine services in nine ways: some through
//! `service(8)`, some by their own conventions, each with its own idea of what
//! "running" means. One entry point keeps the answers consistent and makes the
//! dangerous operations take a snapshot first, every time, rather than when
//! someone remembers.
//!
//!   hajimectl status              what is up, what is not
//!   hajimectl check               the boot self-check, on demand
//!   hajimectl start|stop [name]   all services, or one
//!   hajimectl restart <name>
//!   hajimectl snapshot <reason>   a boot environment before something risky
//!   hajimectl snapshots           what you can go back to
//!   hajimectl save                stop the optional services, keep the sites up
//!   hajimectl resume              start them again

use hajime_sys::{jail, readiness, service, snapshot};
use std::process::{Command, ExitCode};

const BACKUP_MARKER: &str = "/vault/hajime/last-backup";

fn usage() -> ExitCode {
    eprintln!(
        "usage:
  hajimectl status
  hajimectl check
  hajimectl start [service]
  hajimectl stop [service]
  hajimectl restart <service>
  hajimectl snapshot <reason>
  hajimectl snapshots
  hajimectl save
  hajimectl resume
  hajimectl jails
  hajimectl jail-snapshot <jail> <reason>
  hajimectl jail-rollback <jail> <snapshot>"
    );
    ExitCode::from(2)
}

/// Ask rc.d whether a service is running.
fn rc_status(name: &str) -> bool {
    Command::new("service")
        .args([name, "onestatus"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn rc_do(name: &str, action: &str) -> Result<(), String> {
    let out = Command::new("service")
        .args([name, action])
        .output()
        .map_err(|e| format!("could not run service({name}): {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn cmd_status() -> ExitCode {
    println!("{:<20} {:<10} {:<8} STATE", "SERVICE", "TIER", "PORT");
    println!("{}", "-".repeat(58));
    let mut down_essential = 0;

    for s in service::start_order() {
        let running = rc_status(s.rc_name());
        let listening = s.port.map(|p| readiness::port_open(p, std::time::Duration::from_millis(500)));
        let state = match (running, listening) {
            (true, Some(true)) | (true, None) => "up",
            (true, Some(false)) => "started, not listening",
            (false, Some(true)) => "listening, not via rc",
            (false, _) => "down",
        };
        if state == "down" && s.is_essential() {
            down_essential += 1;
        }
        println!(
            "{:<20} {:<10} {:<8} {}",
            s.name,
            if s.is_essential() { "essential" } else { "optional" },
            s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            state
        );
    }

    println!();
    if down_essential > 0 {
        println!("{down_essential} essential service(s) down");
        return ExitCode::FAILURE;
    }
    println!("all essential services up");
    ExitCode::SUCCESS
}

fn cmd_check() -> ExitCode {
    let marker = std::path::Path::new(BACKUP_MARKER);
    let report = readiness::run(marker.exists().then_some(marker));
    print!("{}", report.render());
    if report.ready {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn cmd_start(only: Option<&str>) -> ExitCode {
    let mut failed = 0;
    for s in service::start_order() {
        if only.is_some_and(|n| n != s.name) {
            continue;
        }
        if rc_status(s.rc_name()) {
            println!("  already up   {}", s.name);
            continue;
        }
        match rc_do(s.rc_name(), "start") {
            Ok(()) => println!("  started      {}", s.name),
            Err(e) => {
                failed += 1;
                println!("  FAILED       {}: {e}", s.name);
            }
        }
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn cmd_stop(only: Option<&str>) -> ExitCode {
    // Reverse order so dependants go down before what they depend on.
    for s in service::stop_order() {
        if only.is_some_and(|n| n != s.name) {
            continue;
        }
        match rc_do(s.rc_name(), "stop") {
            Ok(()) => println!("  stopped      {}", s.name),
            Err(e) => println!("  note         {}: {e}", s.name),
        }
    }
    ExitCode::SUCCESS
}

fn cmd_snapshot(reason: &str) -> ExitCode {
    if !snapshot::boot_environments_available() {
        eprintln!("bectl is unavailable: this is not a ZFS root, so there is no");
        eprintln!("boot environment to create. Nothing was changed.");
        return ExitCode::FAILURE;
    }
    match snapshot::create_boot_environment(reason) {
        Ok(name) => {
            println!("created boot environment: {name}");
            println!("to go back: bectl activate {name} && reboot");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_snapshots() -> ExitCode {
    match snapshot::list_boot_environments() {
        Ok(list) if list.is_empty() => {
            println!("no boot environments");
            ExitCode::SUCCESS
        }
        Ok(list) => {
            println!("boot environments:");
            for n in list {
                println!("  {n}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_save() -> ExitCode {
    let optional = service::optional();
    println!(
        "stopping {} optional service(s), about {} MB",
        optional.len(),
        service::reclaimable_mb()
    );
    println!("the sites stay up: nothing essential is touched\n");

    for s in optional.iter().rev() {
        match rc_do(s.rc_name(), "stop") {
            Ok(()) => println!("  stopped      {}  (~{} MB)", s.name, s.typical_mb),
            Err(e) => println!("  note         {}: {e}", s.name),
        }
    }
    println!("\nrun `hajimectl resume` to bring them back");
    ExitCode::SUCCESS
}

fn cmd_resume() -> ExitCode {
    let mut failed = 0;
    for s in service::optional() {
        match rc_do(s.rc_name(), "start") {
            Ok(()) => println!("  started      {}", s.name),
            Err(e) => {
                failed += 1;
                println!("  FAILED       {}: {e}", s.name);
            }
        }
    }
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn cmd_jails() -> ExitCode {
    let running = match jail::running() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot ask about jails: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Width from the data rather than a guess, so a longer service list does
    // not push the last column out of line.
    let holds_w = jail::JAILS.iter().map(|j| j.holds.len()).max().unwrap_or(10);

    println!(
        "{:<6} {:<11} {:<holds_w$} dataset",
        "jail", "state", "holds"
    );
    for j in jail::JAILS {
        println!(
            "{:<6} {:<11} {:<holds_w$} {}",
            j.name,
            jail::state_of(j, &running).label(),
            j.holds,
            j.dataset()
        );
    }

    // Snapshot counts, because a jail with no snapshot has no rollback, and
    // that is the one fact this command exists to surface.
    println!();
    for j in jail::JAILS {
        // A jail that was never created has no dataset, and asking zfs about
        // one produces an error that reads like a fault. The line above already
        // said it does not exist; saying it twice in worse words helps nobody.
        if jail::state_of(j, &running) == jail::State::Missing {
            println!("  {:<6} not created yet", j.name);
            continue;
        }
        match jail::snapshots(j) {
            Ok(list) if list.is_empty() => {
                println!("  {:<6} no snapshots: nothing to roll back to", j.name)
            }
            Ok(list) => {
                let newest = list.last().map(String::as_str).unwrap_or("");
                println!("  {:<6} {} snapshot(s), newest {}", j.name, list.len(), newest);
            }
            Err(e) => println!("  {:<6} {e}", j.name),
        }
    }
    ExitCode::SUCCESS
}

fn cmd_jail_snapshot(name: &str, reason: &str) -> ExitCode {
    let Some(j) = jail::find(name) else {
        eprintln!("no jail named '{name}'. Known jails:");
        for j in jail::JAILS {
            eprintln!("  {:<6} {}", j.name, j.description);
        }
        return ExitCode::from(2);
    };
    match jail::snapshot(j, reason) {
        Ok(full) => {
            println!("{full}");
            println!("to undo whatever you are about to do:");
            println!("  hajimectl jail-rollback {} {full}", j.name);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not snapshot {}: {e}", j.name);
            ExitCode::FAILURE
        }
    }
}

fn cmd_jail_rollback(name: &str, snap: &str) -> ExitCode {
    let Some(j) = jail::find(name) else {
        eprintln!("no jail named '{name}'");
        return ExitCode::from(2);
    };
    let running = match jail::running() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot ask which jails are running: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Say what will be lost before doing it. `zfs rollback -r` destroys every
    // snapshot newer than the target, and that is not recoverable.
    if let Ok(list) = jail::snapshots(j) {
        let target = if snap.contains('@') {
            snap.to_string()
        } else {
            format!("{}@{}", j.dataset(), snap)
        };
        if let Some(pos) = list.iter().position(|s| *s == target) {
            let doomed = list.len() - pos - 1;
            if doomed > 0 {
                println!("rolling back to {target} destroys {doomed} newer snapshot(s):");
                for s in &list[pos + 1..] {
                    println!("  {s}");
                }
            }
        }
    }

    match jail::rollback(j, snap, &running) {
        Ok(()) => {
            println!("{} rolled back to {snap}", j.name);
            println!("start it again with: service jail start {}", j.name);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not roll back {}: {e}", j.name);
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return usage();
    };
    let arg = args.get(1).map(String::as_str);

    // Reject an unknown service name before doing anything, so a typo cannot
    // silently become "operate on everything".
    if matches!(command, "start" | "stop" | "restart") {
        if let Some(name) = arg {
            if service::find(name).is_none() {
                eprintln!("no service named '{name}'. Known services:");
                for s in service::start_order() {
                    eprintln!("  {:<20} {}", s.name, s.description);
                }
                return ExitCode::from(2);
            }
        }
    }

    match command {
        "status" => cmd_status(),
        "check" => cmd_check(),
        "start" => cmd_start(arg),
        "stop" => cmd_stop(arg),
        "restart" => match arg {
            Some(name) => {
                let _ = cmd_stop(Some(name));
                cmd_start(Some(name))
            }
            None => {
                eprintln!("restart needs a service name; use stop then start for everything");
                ExitCode::from(2)
            }
        },
        "snapshot" => match arg {
            Some(reason) => cmd_snapshot(reason),
            None => {
                eprintln!("snapshot needs a reason, for example:");
                eprintln!("  hajimectl snapshot before-postgres-upgrade");
                ExitCode::from(2)
            }
        },
        "snapshots" => cmd_snapshots(),
        "save" => cmd_save(),
        "resume" => cmd_resume(),
        "jails" => cmd_jails(),
        "jail-snapshot" => match (arg, args.get(2).map(String::as_str)) {
            (Some(jail), Some(reason)) => cmd_jail_snapshot(jail, reason),
            _ => {
                eprintln!("jail-snapshot needs a jail and a reason, for example:");
                eprintln!("  hajimectl jail-snapshot ai before-model-swap");
                ExitCode::from(2)
            }
        },
        "jail-rollback" => match (arg, args.get(2).map(String::as_str)) {
            (Some(jail), Some(snap)) => cmd_jail_rollback(jail, snap),
            _ => {
                eprintln!("jail-rollback needs a jail and a snapshot:");
                eprintln!("  hajimectl jails                    to see the jails");
                eprintln!("  hajimectl jail-rollback ai <snapshot>");
                ExitCode::from(2)
            }
        },
        _ => usage(),
    }
}
