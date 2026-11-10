use std::{
    process::Command,
    time::{Duration, Instant},
};

use minidumper_child::MinidumperChild;

fn main() {
    // The child is this program started again: it crashes before doing anything else
    if std::env::args().nth(1).as_deref() == Some("child") {
        unsafe { sadness_generator::raise_segfault() };
    }

    let _guard = MinidumperChild::new()
        .on_minidump(|buffer, _path| {
            // Only the app process's own crash may produce a minidump
            println!("{}", String::from_utf8_lossy(&buffer[..20]));
        })
        .spawn()
        .expect("should start crash reporter");

    // The child inherits this process's exception ports on macOS, so its crash
    // goes to the crash handler attached above first
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("child")
        .spawn()
        .expect("should spawn child");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("should wait on child") {
            println!("child ended: {}", describe(status));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    child.kill().ok();
    child.wait().ok();
    println!("child still running after 10s");
}

#[cfg(unix)]
fn describe(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(signal) => format!("signal {signal}"),
        None => format!("code {:?}", status.code()),
    }
}

#[cfg(not(unix))]
fn describe(status: std::process::ExitStatus) -> String {
    format!("code {:?}", status.code())
}
