use minidumper_child::MinidumperChild;

fn main() {
    // Everything before here runs in both app and crash reporter processes
    let _guard = MinidumperChild::new()
        .on_minidump(|buffer, _path| {
            // Output the first 20 bytes of the minidump to stdio to be checked in the test
            println!("{}", String::from_utf8_lossy(&buffer[..20]));
        })
        .spawn();
    // Everything after here runs in only the app process

    // Wait for longer than the default server timeout
    std::thread::sleep(std::time::Duration::from_secs(10));

    unsafe { sadness_generator::raise_segfault() };
}
