use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
fn test_example_app() {
    let status = Command::new("cargo")
        .args(["build", "--example", "test", "--release"])
        .status()
        .expect("should build test app");
    assert!(status.success());

    // Run the binary directly so its exit status is visible
    let output = Command::new(example_path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn test app")
        .wait_with_output()
        .expect("failed to wait on test app");

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

// A child process inherits the task exception ports on macOS, so a child that
// crashes after the app attached its handler reports to that handler first.
// The handler must let the child die rather than resume it, and must not
// write a minidump for it.
#[cfg(target_os = "macos")]
#[test]
fn test_crashing_child_dies() {
    let status = Command::new("cargo")
        .args(["build", "--example", "crashing_child", "--release"])
        .status()
        .expect("should build crashing child app");
    assert!(status.success());

    let output = Command::new(example_path_named("crashing_child"))
        .stdout(Stdio::piped())
        .spawn()
        .expect("should spawn crashing child app")
        .wait_with_output()
        .expect("failed to wait on crashing child app");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let died = [
        format!("child ended: signal {}", libc::SIGSEGV),
        format!("child ended: signal {}", libc::SIGBUS),
    ];

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        lines.len(),
        1,
        "only the child's end is reported: {lines:?}"
    );
    assert!(died.iter().any(|line| line == lines[0]), "{lines:?}");
}

fn example_path() -> PathBuf {
    example_path_named("test")
}

fn example_path_named(name: &str) -> PathBuf {
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let path = target_dir
        .join("release")
        .join("examples")
        .join(format!("{name}{}", env::consts::EXE_SUFFIX));
    assert!(fs::metadata(&path).is_ok(), "missing {}", path.display());
    path
}
