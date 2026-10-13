use std::collections::BTreeSet;
use std::thread;
use std::time::Duration;

use anyhow::bail;
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
    kill_processes_pipeline(
        catalog,
        pids,
        options,
        can_escalate_with_sudo(),
        try_terminate,
        force_kill,
        default_sudo_kill,
    )
}

fn kill_processes_pipeline<T, F, S>(
    catalog: &mut ProcessCatalog,
    pids: &BTreeSet<Pid>,
    options: &KillOptions,
    can_escalate: bool,
    mut terminator: T,
    mut force_killer: F,
    mut sudo_killer: S,
) -> AppResult<()>
where
    T: FnMut(&sysinfo::Process) -> bool,
    F: FnMut(&mut ProcessCatalog, Pid) -> bool,
    S: FnMut(Pid, i32) -> bool,
{
    if pids.is_empty() {
        bail!("no matching processes found");
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
        let attempted_graceful = !options.force && terminator(process);
        let killed = if attempted_graceful {
            wait_until_exit_or_timeout(
                catalog,
                *pid,
                Duration::from_millis(options.force_after_timeout),
            ) || force_killer(catalog, *pid)
        } else {
            force_killer(catalog, *pid)
        };

        let mut exited = if killed {
            wait_until_exit_or_timeout(catalog, *pid, Duration::from_millis(options.wait_for_exit))
        } else {
            false
        };

        if !exited {
            catalog.refresh();
            if catalog.system().process(*pid).is_none() {
                exited = true;
            }
        }

        let mut tried_sudo = false;
        let mut sudo_killed = false;
        let mut sudo_mode = "sudo (force)";
        let mut used_sudo = false;

        if !exited && can_escalate {
            tried_sudo = true;
            if !options.silent {
                eprintln!(
                    "Failed to kill {} ({name}) with current permissions, escalating with sudo...",
                    pid.as_u32()
                );
            }

            if !options.force && sudo_killer(*pid, 15) {
                sudo_killed = true;
                if wait_until_exit_or_timeout(
                    catalog,
                    *pid,
                    Duration::from_millis(options.force_after_timeout),
                ) {
                    sudo_mode = "sudo (graceful)";
                    exited = true;
                    used_sudo = true;
                }
            }

            if !exited {
                if sudo_killer(*pid, 9) {
                    sudo_killed = true;
                }
                if wait_until_exit_or_timeout(
                    catalog,
                    *pid,
                    Duration::from_millis(options.wait_for_exit),
                ) {
                    exited = true;
                    used_sudo = true;
                }
            }
        }

        if exited {
            killed_any = true;
            if !options.silent {
                let mode = if used_sudo {
                    sudo_mode
                } else if options.force || !attempted_graceful {
                    "force"
                } else {
                    "graceful"
                };
                println!("Killed {} ({name}) via {mode}", pid.as_u32());
            }
        } else if tried_sudo {
            if sudo_killed {
                warnings.push(format!(
                    "{} ({name}, sudo kill sent but exit was not confirmed in time)",
                    pid.as_u32()
                ));
            } else {
                failed.push(format!(
                    "{} ({name}, failed to kill even with sudo)",
                    pid.as_u32()
                ));
            }
        } else if killed {
            warnings.push(format!(
                "{} ({name}, kill sent but exit was not confirmed in time)",
                pid.as_u32()
            ));
        } else {
            failed.push(format!("{} ({name}, failed to send kill)", pid.as_u32()));
        }
    }

    if !failed.is_empty() {
        bail!("failed to kill: {}", failed.join(", "));
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
fn default_sudo_kill(pid: Pid, signal: i32) -> bool {
    let status = std::process::Command::new("sudo")
        .arg("kill")
        .arg(format!("-{signal}"))
        .arg(pid.as_u32().to_string())
        .status();

    match status {
        Ok(exit_status) => exit_status.success(),
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn default_sudo_kill(_pid: Pid, _signal: i32) -> bool {
    false
}

#[cfg(unix)]
fn can_escalate_with_sudo() -> bool {
    !is_root()
}

#[cfg(not(unix))]
fn can_escalate_with_sudo() -> bool {
    false
}

#[cfg(unix)]
fn is_root() -> bool {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() == 0 }
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
        current = current.saturating_mul(2).min(1_000);
    }

    intervals
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use sysinfo::Pid;

    use super::{KillOptions, backoff_intervals, kill_processes_pipeline};
    use crate::process::ProcessCatalog;

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

    #[cfg(unix)]
    #[test]
    fn sudo_escalation_triggers_when_standard_kill_fails() {
        use std::process::Command;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn sleep child process");
        let pid = Pid::from_u32(child.id());

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let pids = BTreeSet::from([pid]);
        let options = KillOptions {
            force: false,
            silent: true,
            force_after_timeout: 50,
            wait_for_exit: 1_000,
        };

        let sudo_called = Arc::new(AtomicBool::new(false));
        let sudo_signal = Arc::new(AtomicI32::new(0));

        let sudo_called_clone = Arc::clone(&sudo_called);
        let sudo_signal_clone = Arc::clone(&sudo_signal);

        let result = kill_processes_pipeline(
            &mut catalog,
            &pids,
            &options,
            true,
            |_proc| false,
            |_cat, _p| false,
            move |_p, sig| {
                sudo_called_clone.store(true, Ordering::SeqCst);
                sudo_signal_clone.store(sig, Ordering::SeqCst);
                let _ = child.kill();
                let _ = child.wait();
                true
            },
        );

        assert!(result.is_ok());
        assert!(sudo_called.load(Ordering::SeqCst));
        assert_eq!(sudo_signal.load(Ordering::SeqCst), 15);
    }

    #[cfg(unix)]
    #[test]
    fn sudo_escalation_force_mode_passes_kill_signal() {
        use std::process::Command;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicI32, Ordering};

        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn sleep child process");
        let pid = Pid::from_u32(child.id());

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let pids = BTreeSet::from([pid]);
        let options = KillOptions {
            force: true,
            silent: true,
            force_after_timeout: 50,
            wait_for_exit: 1_000,
        };

        let sudo_signal = Arc::new(AtomicI32::new(0));
        let sudo_signal_clone = Arc::clone(&sudo_signal);

        let result = kill_processes_pipeline(
            &mut catalog,
            &pids,
            &options,
            true,
            |_proc| false,
            |_cat, _p| false,
            move |_p, sig| {
                sudo_signal_clone.store(sig, Ordering::SeqCst);
                let _ = child.kill();
                let _ = child.wait();
                true
            },
        );

        assert!(result.is_ok());
        assert_eq!(sudo_signal.load(Ordering::SeqCst), 9);
    }

    #[cfg(unix)]
    #[test]
    fn sudo_escalation_reports_error_when_sudo_fails() {
        use std::process::Command;

        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn sleep child process");
        let pid = Pid::from_u32(child.id());

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let pids = BTreeSet::from([pid]);
        let options = KillOptions {
            force: false,
            silent: true,
            force_after_timeout: 50,
            wait_for_exit: 50,
        };

        let result = kill_processes_pipeline(
            &mut catalog,
            &pids,
            &options,
            true,
            |_proc| false,
            |_cat, _p| false,
            |_p, _sig| false,
        );

        let _ = child.kill();
        let _ = child.wait();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to kill even with sudo"));
    }

    #[cfg(unix)]
    #[test]
    fn standard_kill_success_does_not_trigger_sudo() {
        use std::process::Command;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn sleep child process");
        let pid = Pid::from_u32(child.id());

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let pids = BTreeSet::from([pid]);
        let options = KillOptions {
            force: false,
            silent: true,
            force_after_timeout: 500,
            wait_for_exit: 500,
        };

        let sudo_called = Arc::new(AtomicBool::new(false));
        let sudo_called_clone = Arc::clone(&sudo_called);

        let result = kill_processes_pipeline(
            &mut catalog,
            &pids,
            &options,
            true,
            move |_proc| {
                let _ = child.kill();
                let _ = child.wait();
                true
            },
            |_cat, _p| true,
            move |_p, _sig| {
                sudo_called_clone.store(true, Ordering::SeqCst);
                false
            },
        );

        assert!(result.is_ok());
        assert!(!sudo_called.load(Ordering::SeqCst));
    }
}
