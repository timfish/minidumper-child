use minidumper_child::MinidumperChild;

fn main() {
    // Everything before here runs in both app and crash reporter processes
    let guard = MinidumperChild::new()
        .on_crash(|crash_context, sender| {
            // Runs in the crashing app process. Only signal-safe work is
            // allowed here, so send a fixed-size buffer from the stack.
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let thread_id = crash_context.tid as u64;
            #[cfg(target_os = "windows")]
            let thread_id = crash_context.thread_id as u64;
            #[cfg(target_os = "macos")]
            let thread_id = crash_context.thread as u64;

            sender.send_message(7, &thread_id.to_le_bytes()).ok();
        })
        .on_message(|kind, buffer| {
            // The crash reporter inherits the environment of the app
            if std::env::var_os("TEST_SLOW_MESSAGES").is_some() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            // Arrives in the crash reporter before on_minidump
            println!("message kind={kind} len={}", buffer.len());
        })
        .on_minidump(|buffer, _path| {
            // Output the first 20 bytes of the minidump to stdio to be checked in the test
            println!("{}", String::from_utf8_lossy(&buffer[..20]));
        })
        .spawn()
        .expect("should start crash reporter");
    // Everything after here runs in only the app process

    let arg = std::env::args().nth(1);

    match arg.as_deref() {
        // Waits for the test to kill this process
        Some("--wait") => {
            println!("ready");
            std::thread::sleep(std::time::Duration::from_secs(60));
            return;
        }
        // Starts a process that keeps the socket to the crash reporter
        // open, then waits for the test to kill this process
        #[cfg(unix)]
        Some("--fork-wait") => {
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                unsafe {
                    // So the test sees stdout close when the crash
                    // reporter exits
                    libc::close(1);
                    libc::sleep(30);
                    libc::_exit(0);
                }
            }
            println!("ready {pid}");
            std::thread::sleep(std::time::Duration::from_secs(60));
            return;
        }
        // Sends messages and exits at once
        Some("--send-and-exit") => {
            for kind in 0..10 {
                guard.send_message(kind, b"x").ok();
            }
            return;
        }
        _ => {}
    }

    // Send no messages for longer than the old server stale timeout
    std::thread::sleep(std::time::Duration::from_secs(10));

    unsafe { sadness_generator::raise_segfault() };
}
