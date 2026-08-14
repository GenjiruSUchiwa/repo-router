use std::process::Command;

#[test]
fn test_version_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .arg("version")
        .output()
        .expect("failed to execute rr binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("rr 0.1.0 ("));
    assert!(stdout.ends_with(")\n"));
}

#[test]
fn test_version_flag() {
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .arg("--version")
        .output()
        .expect("failed to execute rr binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("rr 0.1.0 ("));
    assert!(stdout.ends_with(")\n"));
}
