#![cfg(windows)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fuckport::input::Target;
use fuckport::process::ProcessCatalog;
use sysinfo::Pid;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_WNDPROC, IsWindow, MSG,
    PM_REMOVE, PeekMessageW, SetWindowLongPtrW, WM_CLOSE, WS_OVERLAPPEDWINDOW,
};

const CREATE_NO_WINDOW: u32 = 0x08000000;
static IGNORE_CLOSE: AtomicBool = AtomicBool::new(false);

struct Fixture(Child);

impl Fixture {
    fn start(mode: &str, file: Option<&Path>) -> (Self, u16) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "windows_fixture", "--nocapture"])
            .env("FUCKPORT_FIXTURE_MODE", mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .creation_flags(CREATE_NO_WINDOW);
        if let Some(file) = file {
            command.env("FUCKPORT_FIXTURE_FILE", file);
        }
        let mut fixture = Self(command.spawn().expect("failed to start fixture"));
        let stdout = fixture.0.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some((_, port)) = line.split_once("FUCKPORT_READY:") {
                    let _ = sender.send(port.trim().parse::<u16>().unwrap());
                }
            }
        });
        let port = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("fixture did not become ready");
        (fixture, port)
    }

    fn pid(&self) -> Pid {
        Pid::from_u32(self.0.id())
    }

    fn assert_exited(&mut self) {
        assert!(
            self.0
                .try_wait()
                .expect("failed to inspect fixture")
                .is_some()
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("fuckport-windows-{}-{suffix}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        let file = self.0.join("held.txt");
        fs::write(&file, "test fixture").unwrap();
        file
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_cli(args: &[String]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_fuckport"))
        .args(["--force-after-timeout", "100", "--wait-for-exit", "2000"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("failed to run CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn closes_a_window_gracefully() {
    let (mut fixture, _) = Fixture::start("window", None);
    let output = run_cli(&[fixture.pid().as_u32().to_string()]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("via graceful"));
    fixture.assert_exited();
}

#[test]
fn forces_a_window_that_ignores_close_after_timeout() {
    let (mut fixture, _) = Fixture::start("ignore-close", None);
    let output = run_cli(&[fixture.pid().as_u32().to_string()]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("via force"));
    fixture.assert_exited();
}

#[test]
fn force_flag_skips_window_close() {
    let (mut fixture, _) = Fixture::start("window", None);
    let output = run_cli(&["--force".into(), fixture.pid().as_u32().to_string()]);
    assert!(String::from_utf8_lossy(&output.stdout).contains("via force"));
    fixture.assert_exited();
}

#[test]
fn kills_a_process_by_its_listening_port() {
    let (mut fixture, port) = Fixture::start("console", None);
    run_cli(&[format!(":{port}")]);
    fixture.assert_exited();
}

#[test]
fn kills_a_process_holding_a_file() {
    let directory = TestDirectory::new();
    let file = directory.file();
    let (mut fixture, _) = Fixture::start("console", Some(&file));
    run_cli(&["--file".into(), file.to_string_lossy().into_owned()]);
    fixture.assert_exited();
}

#[test]
fn directory_scan_finds_a_process_holding_a_descendant_file() {
    let directory = TestDirectory::new();
    let child = directory.0.join("child");
    fs::create_dir(&child).unwrap();
    let file = child.join("held.txt");
    fs::write(&file, "test fixture").unwrap();
    let (fixture, _) = Fixture::start("console", Some(&file));
    let catalog = ProcessCatalog::load().unwrap();
    let owners = catalog
        .resolve_targets(&[Target::Directory(directory.0.clone())], false)
        .unwrap();
    assert!(owners.contains(&fixture.pid()));
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CLOSE {
        if !IGNORE_CLOSE.load(Ordering::Relaxed) {
            unsafe { DestroyWindow(window) };
        }
        return 0;
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

// Runs only in a dedicated child process; every parent owns and reaps its fixture.
#[test]
#[ignore = "child process fixture"]
fn windows_fixture() {
    let mode = std::env::var("FUCKPORT_FIXTURE_MODE").expect("fixture mode missing");
    let _file = std::env::var_os("FUCKPORT_FIXTURE_FILE").map(|file| fs::File::open(file).unwrap());
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let window = if mode != "console" {
        IGNORE_CLOSE.store(mode == "ignore-close", Ordering::Relaxed);
        // A built-in window class avoids global class registration. No WS_VISIBLE:
        // the fixture stays hidden even on an interactive desktop.
        let window = unsafe {
            CreateWindowExW(
                0,
                windows_sys::core::w!("STATIC"),
                windows_sys::core::w!("fuckport test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!window.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { SetWindowLongPtrW(window, GWLP_WNDPROC, window_proc as *const () as isize) };
        window
    } else {
        std::ptr::null_mut()
    };
    println!("\nFUCKPORT_READY:{}", listener.local_addr().unwrap().port());
    std::io::stdout().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !window.is_null() {
            let mut message = MSG::default();
            while unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0
            {
                unsafe { DispatchMessageW(&message) };
            }
            if unsafe { IsWindow(window) } == 0 {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("fixture was not terminated within 30 seconds");
}
