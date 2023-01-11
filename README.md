# `minidumper-child` 

![Master branch integration test status](https://img.shields.io/github/actions/workflow/status/timfish/minidumper-child/test.yml?label=Integration%20Tests&style=for-the-badge)

Essentially takes the code from the `minidumper` [diskwrite
example](https://github.com/EmbarkStudios/crash-handling/blob/main/minidumper/examples/diskwrite.rs)
and packages it in reusable form with some integration tests. This wraps the
`minidumper` and `crash-handler` crates to capture and send minidumps from a
separate crash reporting process. 

It spawns the current executable again with an argument that causes it to start
in crash reporter mode. In this mode it waits for minidump notification from the
main app process and passes the minidump file to a user defined closure.

```toml
[dependencies]
minidumper-child = "0.1"
```

```rust
use minidumper_child::MinidumperChild;

fn main() {
    // Everything before here runs in both app and crash reporter processes
    let _guard = MinidumperChild::new()
        .on_minidump(|buffer: Vec<u8>, path: &Path| {
            // Do something with the minidump file here
        })
        .spawn();
    // Everything after here will only run in the app process

    App::run();

    // This will cause on_minidump to be called in the crash reporter process 
    #[allow(deref_nullptr)]
    unsafe {
        *std::ptr::null_mut() = true;
    }
}
```
