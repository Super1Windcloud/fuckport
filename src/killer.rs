use std::collections::BTreeSet;
use std::thread;
use std::time::Duration;

use sysinfo::Pid;
#[cfg(unix)]
use sysinfo::Signal;

use crate::error::AppResult;
use crate::process::ProcessCatalog;

pub struct KillOptions {
    pub force: bool,
    pub silent: bool,
    pub force_after_timeout: u64,
    pub wait_for_exit: u64,
}

pub fn kill_processes(
    catalog: &mut ProcessCatalog,
    pids: &BTreeSet<Pid>,
    options: &KillOptions,
) -> AppResult<()> {
    if pids.is_empty() {
        return Err("no matching processes found".to_string());
    }

    let mut failed = Vec::new();
    let mut warnings = Vec::new();
    let mut killed_any = false;

    for pid in pids {
        if *pid == catalog.current_pid() {
            failed.push(format!(
                "{} (refusing to kill current process)",
                pid.as_u32()
            ));
            continue;
        }

        catalog.refresh();
        let Some(process) = catalog.system().process(*pid) else {
            continue;
        };

        let name = process.name().to_string_lossy().into_owned();
        let attempted_graceful = !options.force && try_terminate(process);
        let killed = if attempted_graceful {
            wait_until_exit_or_timeout(
                catalog,
                *pid,
                Duration::from_millis(options.force_after_timeout),
            ) || force_kill(catalog, *pid)
        } else {
            force_kill(catalog, *pid)
        };

        let exited = if killed {
            wait_until_exit_or_timeout(catalog, *pid, Duration::from_millis(options.wait_for_exit))
        } else {
            false
        };

        if exited {
            killed_any = true;
            if !options.silent {
                let mode = if options.force || !attempted_graceful {
                    "force"
                } else {
                    "graceful"
                };
                println!("Killed {} ({name}) via {mode}", pid.as_u32());
            }
        } else {
            if killed {
                warnings.push(format!(
                    "{} ({name}, kill sent but exit was not confirmed in time)",
                    pid.as_u32()
                ));
            } else {
                failed.push(format!("{} ({name}, failed to send kill)", pid.as_u32()));
            }
        }
    }

    if !failed.is_empty() {
        return Err(format!("failed to kill: {}", failed.join(", ")));
    }

    if !warnings.is_empty() && !options.silent {
        eprintln!("Warning: {}", warnings.join(", "));
    }

    if !killed_any && !options.silent {
        println!("No processes were terminated.");
    }

    Ok(())
}

#[cfg(unix)]
fn try_terminate(process: &sysinfo::Process) -> bool {
    process.kill_with(Signal::Term).unwrap_or(false)
}

#[cfg(not(unix))]
fn try_terminate(_process: &sysinfo::Process) -> bool {
    false
}

fn force_kill(catalog: &mut ProcessCatalog, pid: Pid) -> bool {
    catalog.refresh();
    match catalog.system().process(pid) {
        Some(process) => process.kill(),
        None => true,
    }
}

fn wait_until_exit_or_timeout(catalog: &mut ProcessCatalog, pid: Pid, timeout: Duration) -> bool {
    for sleep_ms in backoff_intervals(timeout) {
        catalog.refresh();
        if catalog.system().process(pid).is_none() {
            return true;
        }
        thread::sleep(Duration::from_millis(sleep_ms));
    }

    catalog.refresh();
    catalog.system().process(pid).is_none()
}

fn backoff_intervals(timeout: Duration) -> Vec<u64> {
    let timeout_ms = timeout.as_millis() as u64;
    if timeout_ms == 0 {
        return Vec::new();
    }

    let mut intervals = Vec::new();
    let mut elapsed = 0_u64;
    let mut current = 50_u64;

    while elapsed < timeout_ms {
        let next = current.min(timeout_ms - elapsed);
        intervals.push(next);
        elapsed += next;
        current = (current.saturating_mul(2)).min(1_000);
    }

    intervals
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::backoff_intervals;

    #[test]
    fn backoff_stays_within_timeout() {
        let intervals = backoff_intervals(Duration::from_millis(1_500));
        assert_eq!(intervals.iter().sum::<u64>(), 1_500);
    }

    #[test]
    fn backoff_grows_then_caps() {
        let intervals = backoff_intervals(Duration::from_millis(3_000));
        assert_eq!(&intervals[..5], &[50, 100, 200, 400, 800]);
        assert!(intervals.iter().all(|value| *value <= 1_000));
    }

    #[test]
    fn zero_timeout_has_no_wait_intervals() {
        let intervals = backoff_intervals(Duration::from_millis(0));
        assert!(intervals.is_empty());
    }
}
