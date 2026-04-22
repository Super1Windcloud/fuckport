use std::process::Command;

#[test]
fn help_mentions_timeout_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_fuckport"))
        .arg("--help")
        .output()
        .expect("failed to run fuckport --help");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--force-after-timeout"));
    assert!(stdout.contains("--wait-for-exit"));
}
