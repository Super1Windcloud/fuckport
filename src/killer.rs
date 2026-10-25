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
    F: FnMut(&mut ProcessCatalog, Pid) -> AppResult<bool>,
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

        if process.status() == sysinfo::ProcessStatus::Zombie
            || process.status() == sysinfo::ProcessStatus::Dead
        {
            let parent_info = process
                .parent()
                .map(|p| format!(" (terminate its parent PID {} to reap it)", p.as_u32()))
                .unwrap_or_default();
            failed.push(format!(
                "{} ({name}) is a zombie/defunct process and cannot be killed directly{parent_info}",
                pid.as_u32()
            ));
            continue;
        }

        if let Some(expected_start) = catalog.initial_start_time(*pid) {
            if process.start_time() != expected_start {
                failed.push(format!(
                    "{} ({name}, process start time changed from {expected_start} to {}, refusing to kill recycled PID)",
                    pid.as_u32(),
                    process.start_time()
                ));
                continue;
            }
        }

        let attempted_graceful = !options.force && terminator(process);
        let graceful_exit = attempted_graceful
            && wait_until_exit_or_timeout(
                catalog,
                *pid,
                Duration::from_millis(options.force_after_timeout),
            );
        let mut kill_error = None;
        let killed = graceful_exit
            || match force_killer(catalog, *pid) {
                Ok(killed) => killed,
                Err(error) => {
                    kill_error = Some(format!("{error:#}"));
                    false
                }
            };

        let mut exited = if killed {
            wait_until_exit_or_timeout(catalog, *pid, Duration::from_millis(options.wait_for_exit))
        } else {
            false
        };

        if !exited && !catalog.refresh_pid(*pid) {
            exited = true;
        }

        // Errors include process identity mismatches; never bypass them through sudo.
        if !exited && let Some(reason) = kill_error {
            failed.push(format!("{} ({name}, {reason})", pid.as_u32()));
            continue;
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
                } else if graceful_exit {
                    "graceful"
                } else {
                    "force"
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

#[cfg(windows)]
fn try_terminate(process: &sysinfo::Process) -> bool {
    request_window_close(process.pid())
}

#[cfg(windows)]
fn request_window_close(pid: Pid) -> bool {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    struct CloseRequest {
        pid: u32,
        sent: bool,
    }

    unsafe extern "system" fn close_window(window: HWND, context: LPARAM) -> i32 {
        // EnumWindows invokes this callback synchronously while the request is alive.
        let request = unsafe { &mut *(context as *mut CloseRequest) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == request.pid && unsafe { PostMessageW(window, WM_CLOSE, 0, 0) } != 0 {
            request.sent = true;
        }
        1
    }

    let mut request = CloseRequest {
        pid: pid.as_u32(),
        sent: false,
    };
    unsafe {
        EnumWindows(
            Some(close_window),
            &mut request as *mut CloseRequest as LPARAM,
        )
    };
    request.sent
}

#[cfg(not(any(unix, windows)))]
fn try_terminate(_process: &sysinfo::Process) -> bool {
    false
}

fn force_kill(catalog: &mut ProcessCatalog, pid: Pid) -> AppResult<bool> {
    if !catalog.refresh_pid(pid) {
        return Ok(true);
    }
    let process = catalog
        .system()
        .process(pid)
        .expect("refreshed process exists");
    let expected_start = catalog.initial_start_time(pid);
    if expected_start.is_some_and(|start| start != process.start_time()) {
        bail!("refusing to kill recycled PID {}", pid.as_u32());
    }

    #[cfg(windows)]
    return windows_force_kill(pid, expected_start).or_else(|error| {
        if !is_windows_access_denied(&error) {
            return Err(error);
        }
        let Some(start_time) = expected_start.filter(|start| *start != 0) else {
            return Err(error);
        };
        let mut command = match windows_sudo_command(pid, start_time) {
            Ok(command) => command,
            Err(reason) => {
                return Err(error.context(format!("cannot prepare sudo retry: {reason}")));
            }
        };
        eprintln!(
            "Access denied for PID {}; requesting elevation with sudo...",
            pid.as_u32()
        );
        match command.status() {
            Ok(status) if status.success() => Ok(true),
            Ok(status) => Err(error.context(format!(
                "sudo retry failed ({status}); protected processes may still deny termination"
            ))),
            Err(reason) => Err(error.context(format!("sudo retry unavailable: {reason}"))),
        }
    });

    #[cfg(not(windows))]
    Ok(process.kill())
}

#[cfg(windows)]
fn windows_sudo_command(pid: Pid, start_time: u64) -> AppResult<std::process::Command> {
    let mut command = std::process::Command::new("sudo.exe");
    command
        .arg(std::env::current_exe()?)
        .arg("--elevated-kill")
        .arg(pid.as_u32().to_string())
        .arg(start_time.to_string());
    Ok(command)
}

/// Internal entrypoint for the elevated helper; never retries through sudo.
#[cfg(windows)]
#[doc(hidden)]
pub fn kill_elevated_windows_process(pid: Pid, start_time: u64) -> AppResult<()> {
    if pid.as_u32() == std::process::id() || start_time == 0 {
        bail!("invalid elevated kill identity");
    }
    windows_force_kill(pid, Some(start_time)).map(|_| ())
}

#[cfg(windows)]
fn is_windows_access_denied(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        == Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
}

#[cfg(windows)]
fn windows_force_kill(pid: Pid, expected_start: Option<u64>) -> AppResult<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        TerminateProcess,
    };

    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid.as_u32(),
        )
    };
    if handle.is_null() {
        return Err(windows_kill_error(std::io::Error::last_os_error()));
    }

    let result = (|| {
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } == 0
        {
            return Err(windows_kill_error(std::io::Error::last_os_error()));
        }
        let ticks = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let start = (ticks / 10_000_000).saturating_sub(11_644_473_600);
        if expected_start.is_some_and(|expected| expected != start) {
            bail!("refusing to kill recycled PID {}", pid.as_u32());
        }
        if unsafe { TerminateProcess(handle, 1) } == 0 {
            return Err(windows_kill_error(std::io::Error::last_os_error()));
        }
        Ok(true)
    })();
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(windows)]
fn windows_kill_error(error: std::io::Error) -> anyhow::Error {
    let error = anyhow::Error::new(error);
    if is_windows_access_denied(&error) {
        error.context("access denied; run fuckport from a terminal opened with 'Run as administrator'; protected processes may still deny termination")
    } else {
        error.context("failed to terminate Windows process")
    }
}

fn wait_until_exit_or_timeout(catalog: &mut ProcessCatalog, pid: Pid, timeout: Duration) -> bool {
    for sleep_ms in backoff_intervals(timeout) {
        if !catalog.refresh_pid(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(sleep_ms));
    }

    !catalog.refresh_pid(pid)
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

    fn sleep_child() -> std::process::Child {
        #[cfg(unix)]
        let mut command = std::process::Command::new("sleep");
        #[cfg(unix)]
        command.arg("30");
        #[cfg(windows)]
        let mut command = {
            use std::os::windows::process::CommandExt;
            let mut command = std::process::Command::new("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            command
        };
        command.spawn().expect("failed to spawn test child")
    }

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

    #[cfg(windows)]
    #[test]
    fn sudo_command_preserves_exact_process_identity() {
        let command = super::windows_sudo_command(Pid::from_u32(123), 456).unwrap();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(command.get_program(), "sudo.exe");
        assert_eq!(args[0], std::env::current_exe().unwrap());
        assert_eq!(&args[1..], &["--elevated-kill", "123", "456"]);
    }

    #[cfg(windows)]
    #[test]
    fn only_access_denied_allows_elevation() {
        assert!(super::is_windows_access_denied(&super::windows_kill_error(
            std::io::Error::from_raw_os_error(5)
        )));
        assert!(!super::is_windows_access_denied(
            &super::windows_kill_error(std::io::Error::from_raw_os_error(87))
        ));
        assert!(!super::is_windows_access_denied(&anyhow::anyhow!(
            "refusing to kill recycled PID"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn access_denied_includes_elevation_guidance_and_original_error() {
        let error = super::windows_kill_error(std::io::Error::from_raw_os_error(5));
        assert!(error.to_string().contains("Run as administrator"));
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(5)
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_kill_checks_creation_time_before_terminating() {
        let mut child = sleep_child();
        let result = super::windows_force_kill(Pid::from_u32(child.id()), Some(1));
        let still_running = child.try_wait().expect("failed to inspect child").is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(result.unwrap_err().to_string().contains("recycled PID"));
        assert!(still_running);
    }

    #[test]
    fn force_kill_errors_keep_the_underlying_reason() {
        let mut child = sleep_child();
        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let result = kill_processes_pipeline(
            &mut catalog,
            &BTreeSet::from([Pid::from_u32(child.id())]),
            &KillOptions {
                force: true,
                silent: true,
                force_after_timeout: 0,
                wait_for_exit: 0,
            },
            false,
            |_| panic!("force must skip graceful termination"),
            |_, _| anyhow::bail!("test permission failure"),
            |_, _| panic!("escalation is disabled"),
        );
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("test permission failure")
        );
    }

    #[test]
    fn sudo_escalation_triggers_when_standard_kill_fails() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

        let mut child = sleep_child();
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
            |_cat, _p| Ok(false),
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

    #[test]
    fn sudo_escalation_force_mode_passes_kill_signal() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicI32, Ordering};

        let mut child = sleep_child();
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
            |_cat, _p| Ok(false),
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

    #[test]
    fn sudo_escalation_reports_error_when_sudo_fails() {
        let mut child = sleep_child();
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
            |_cat, _p| Ok(false),
            |_p, _sig| false,
        );

        let _ = child.kill();
        let _ = child.wait();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to kill even with sudo"));
    }

    #[test]
    fn standard_kill_success_does_not_trigger_sudo() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut child = sleep_child();
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
            |_cat, _p| Ok(true),
            move |_p, _sig| {
                sudo_called_clone.store(true, Ordering::SeqCst);
                false
            },
        );

        assert!(result.is_ok());
        assert!(!sudo_called.load(Ordering::SeqCst));
    }

    #[test]
    fn pid_recycling_refuses_to_kill_replaced_process() {
        let mut child = sleep_child();
        let pid = Pid::from_u32(child.id());

        let mut catalog = ProcessCatalog::load().expect("failed to load catalog");
        let pids = BTreeSet::from([pid]);
        let options = KillOptions {
            force: false,
            silent: true,
            force_after_timeout: 50,
            wait_for_exit: 50,
        };

        // Mutate initial_start_times so that it simulates another process taking over this PID
        catalog.initial_start_times.insert(pid, 1);

        let result = kill_processes_pipeline(
            &mut catalog,
            &pids,
            &options,
            false,
            |_proc| true,
            |_cat, _p| Ok(true),
            |_p, _sig| false,
        );

        let _ = child.kill();
        let _ = child.wait();

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("refusing to kill recycled PID"));
    }
}
