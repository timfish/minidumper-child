use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::Duration,
};

#[test]
fn test_example_app() {
    build_example();

    // Run the binary directly so its exit status is visible
    let output = Command::new(example_path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app")
        .wait_with_output()
        .expect("failed to wait on test app");

    check_output(&output);
}

// The server times out while the app is stopped. The app must start a new
// server and still write a minidump.
#[cfg(unix)]
#[test]
fn test_example_app_after_server_timeout() {
    build_example();

    let child = Command::new(example_path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app");
    let pid = child.id() as libc::pid_t;

    std::thread::sleep(Duration::from_secs(1));
    // Longer than the default server stale timeout of 5 seconds
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    std::thread::sleep(Duration::from_secs(7));
    unsafe { libc::kill(pid, libc::SIGCONT) };

    let output = child
        .wait_with_output()
        .expect("failed to wait on test app");

    check_output(&output);
}

fn build_example() {
    let status = Command::new("cargo")
        .args(["build", "--example", "test", "--release"])
        .status()
        .expect("should build test app");
    assert!(status.success());
}

fn check_output(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();

    // The message sent from on_crash arrives before the minidump
    assert_eq!(lines.next(), Some("message kind=7 len=8"));

    // minidump files always start with MDMP characters
    assert!(lines.next().is_some_and(|line| line.starts_with("MDMP")));

    // After the dump the app exits before the crash signal kills it
    #[cfg(target_os = "macos")]
    assert_eq!(output.status.code(), Some(1));

    // The crash signal kills the app after the dump
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGSEGV));
    }
}

fn example_path() -> PathBuf {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let path = target_dir
        .join("release")
        .join("examples")
        .join(format!("test{}", env::consts::EXE_SUFFIX));
    assert!(fs::metadata(&path).is_ok(), "missing {}", path.display());
    path
}
