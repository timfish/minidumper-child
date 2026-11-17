use minidumper_child::MinidumperChild;

fn main() {
    // Everything before here runs in both app and crash reporter processes
    let _guard = MinidumperChild::new()
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
            // Arrives in the crash reporter before on_minidump
            println!("message kind={kind} len={}", buffer.len());
        })
        .on_minidump(|buffer, _path| {
            // Output the first 20 bytes of the minidump to stdio to be checked in the test
            println!("{}", String::from_utf8_lossy(&buffer[..20]));
        })
        .spawn();
    // Everything after here runs in only the app process

    // Waits for the test to kill this process
    if std::env::args().any(|arg| arg == "--wait") {
        println!("ready");
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }

    // Send no messages for longer than the old server stale timeout
    std::thread::sleep(std::time::Duration::from_secs(10));

    unsafe { sadness_generator::raise_segfault() };
}
