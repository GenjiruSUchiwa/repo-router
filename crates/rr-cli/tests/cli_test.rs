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
fn version_languages_lists_every_recognised_language() {
    let output = Command::new(env!("CARGO_BIN_EXE_rr"))
        .arg("version")
        .arg("--languages")
        .output()
        .expect("failed to execute rr binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 21);
    let rust = lines
        .iter()
        .find(|line| line.starts_with("rust"))
        .expect("rust missing");
    assert!(rust.contains("complete"), "{rust}");
    let csharp = lines
        .iter()
        .find(|line| line.starts_with("csharp"))
        .expect("csharp missing");
    assert!(csharp.contains("tags"), "{csharp}");
    assert!(csharp.ends_with(" 1"), "{csharp}");
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
