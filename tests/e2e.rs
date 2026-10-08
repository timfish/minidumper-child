use std::process::{Command, Stdio};

#[test]
fn test_example_app() {
    let output = Command::new("cargo")
        .args(["run", "--example", "test", "--release"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn tests app")
        .wait_with_output()
        .expect("failed to wait on test app");

    let output = String::from_utf8_lossy(&output.stdout);
    let mut lines = output.lines();

    // The message sent from on_crash arrives before the minidump
    assert_eq!(lines.next(), Some("message kind=7 len=8"));

    // minidump files always start with MDMP characters
    assert!(lines.next().is_some_and(|line| line.starts_with("MDMP")));
}

#[test]
fn test_snapshot_app() {
    let output = Command::new("cargo")
        .args(["run", "--example", "snapshot", "--release"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn snapshot app")
        .wait_with_output()
        .expect("failed to wait on snapshot app");

    let output = String::from_utf8_lossy(&output.stdout);

    // The ALIVE lines prove the app process survived the snapshots, and the
    // final dump proves the crash reporter stayed alive to serve the crash
    #[cfg(unix)]
    let expected = [
        "MDMP",
        "ALIVE after signal snapshot",
        "MDMP",
        "ALIVE after capture_minidump",
        "MDMP",
    ]
    .as_slice();
    #[cfg(not(unix))]
    let expected = ["MDMP", "ALIVE after capture_minidump", "MDMP"].as_slice();

    assert_eq!(output.lines().collect::<Vec<_>>(), expected);
}
