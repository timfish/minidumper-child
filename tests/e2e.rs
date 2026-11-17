use std::{
    env, fs,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::mpsc,
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

    check_crash_output(&output);
}

// The OS can stop the app for longer than the old server stale timeout,
// for example with App Nap on macOS. The crash reporter must still be
// there when the app crashes.
#[cfg(unix)]
#[test]
fn test_example_app_after_stop() {
    build_example();

    let child = Command::new(example_path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app");
    let pid = child.id() as libc::pid_t;

    std::thread::sleep(Duration::from_secs(1));
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    std::thread::sleep(Duration::from_secs(7));
    unsafe { libc::kill(pid, libc::SIGCONT) };

    let output = child
        .wait_with_output()
        .expect("failed to wait on test app");

    check_crash_output(&output);
}

// The crash reporter must exit soon after the app is killed. Killing the
// app closes its socket, so this covers the socket path.
#[test]
fn test_crash_reporter_exits_with_app() {
    build_example();

    let mut child = Command::new(example_path())
        .arg("--wait")
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app");

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).expect("should read from app");
    assert_eq!(line.trim_end(), "ready");

    child.kill().expect("should kill test app");
    child.wait().expect("failed to wait on test app");

    let rest = read_to_end_within(stdout, Duration::from_secs(5))
        .expect("crash reporter should exit after the app is killed");
    assert_eq!(rest, "");
}

// A forked process keeps the app's socket open after the app is killed, so
// only the app watcher in the crash reporter can end it.
#[cfg(unix)]
#[test]
fn test_crash_reporter_watches_app() {
    build_example();

    let mut child = Command::new(example_path())
        .arg("--fork-wait")
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app");

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).expect("should read from app");
    let fork_pid: libc::pid_t = line
        .trim_end()
        .strip_prefix("ready ")
        .and_then(|pid| pid.parse().ok())
        .expect("app should print the forked pid");

    child.kill().expect("should kill test app");
    child.wait().expect("failed to wait on test app");

    let rest = read_to_end_within(stdout, Duration::from_secs(5));
    unsafe { libc::kill(fork_pid, libc::SIGKILL) };

    let rest = rest.expect("crash reporter should exit after the app is killed");
    assert_eq!(rest, "");
}

// The app sends messages and exits at once. The crash reporter must handle
// every message before it exits, even when each one is slow.
#[test]
fn test_messages_sent_before_exit_arrive() {
    build_example();

    let output = Command::new(example_path())
        .arg("--send-and-exit")
        .env("TEST_SLOW_MESSAGES", "1")
        .stdout(Stdio::piped())
        .output()
        .expect("should run test app");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected: Vec<String> = (0..10)
        .map(|kind| format!("message kind={kind} len=1"))
        .collect();
    assert_eq!(stdout.lines().collect::<Vec<_>>(), expected);
}

/// Reads until the crash reporter exits and closes the shared stdout pipe.
fn read_to_end_within(mut stdout: impl Read + Send + 'static, timeout: Duration) -> Option<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut rest = String::new();
        stdout.read_to_string(&mut rest).ok();
        tx.send(rest).ok();
    });
    rx.recv_timeout(timeout).ok()
}

fn build_example() {
    let status = Command::new("cargo")
        .args(["build", "--example", "test", "--release"])
        .status()
        .expect("should build test app");
    assert!(status.success());
}

fn check_crash_output(output: &Output) {
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
