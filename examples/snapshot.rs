//! Takes minidump snapshots without crashing, then crashes for a final dump.
//! Used by the e2e tests.

use minidumper_child::MinidumperChild;

fn main() {
    // Everything before here runs in both app and crash reporter processes
    let child = MinidumperChild::new().on_minidump(|buffer, _path| {
        // Output the magic bytes of the minidump to stdout to be checked in the test
        println!("{}", String::from_utf8_lossy(&buffer[..4]));
    });

    #[cfg(unix)]
    let child = child.with_snapshot_signal(libc::SIGUSR2);

    let handle = child.spawn().expect("failed to spawn crash reporter");
    // Everything after here runs in only the app process

    // Snapshot via signal, as `kill -USR2 <pid>` would trigger from outside.
    // Delivery and capture are asynchronous, so give it time to complete.
    #[cfg(unix)]
    {
        unsafe { libc::raise(libc::SIGUSR2) };
        std::thread::sleep(std::time::Duration::from_secs(3));
        println!("ALIVE after signal snapshot");
    }

    // Snapshot via the API, which blocks until the dump has been written.
    handle
        .capture_minidump()
        .expect("failed to capture snapshot");
    println!("ALIVE after capture_minidump");

    unsafe { sadness_generator::raise_segfault() };
}
