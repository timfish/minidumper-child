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

## Minidump snapshots without crashing

A minidump of the app process can be captured at any point without crashing or
exiting. The same `on_minidump` closure is called with the dump and the app
carries on running. The crash reporter process stays alive until the app
process exits, so any number of dumps can be captured.

```rust
let handle = MinidumperChild::new()
    .on_minidump(|buffer: Vec<u8>, path: &Path| { /* ... */ })
    .spawn()
    .expect("failed to spawn crash reporter");

// Capture a snapshot of the entire process and block until it's written
handle.capture_minidump().expect("failed to capture minidump");
```

On Unix you can also bind a signal so snapshots can be triggered from outside
the process, for example with `kill -USR2 <pid>` or
`docker kill --signal=USR2 <container>`:

```rust
let handle = MinidumperChild::new()
    .on_minidump(|buffer: Vec<u8>, path: &Path| { /* ... */ })
    .with_snapshot_signal(libc::SIGUSR2)
    .spawn()
    .expect("failed to spawn crash reporter");
```

The snapshot signal must not be one of the fatal signals already handled by
`crash-handler` (`SIGABRT`, `SIGBUS`, `SIGFPE`, `SIGILL`, `SIGSEGV`,
`SIGTRAP`) — those capture a dump too, but always terminate the process
afterwards.
