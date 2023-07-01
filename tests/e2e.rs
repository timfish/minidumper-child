use std::process::{Command, Stdio};

#[test]
fn test_example_app() {
    let output = Command::new("cargo")
        .args(["run", "--example", "test"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn tests app")
        .wait_with_output()
        .expect("failed to wait on test app");

    let output = String::from_utf8_lossy(&output.stdout);

    // minidump files always start with MDMP characters
    assert!(output.starts_with("MDMP"));
}
