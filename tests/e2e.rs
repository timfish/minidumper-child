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
