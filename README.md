# `minidumper-child` 

![Master branch integration test status](https://img.shields.io/github/actions/workflow/status/timfish/minidumper-child/test.yml?label=Integration%20Tests&style=for-the-badge)

Takes the code from the `minidumper` [diskwrite
example](https://github.com/EmbarkStudios/crash-handling/blob/main/minidumper/examples/diskwrite.rs)
and packages it in reusable form with integration tests. It wraps the
`minidumper` and `crash-handler` crates to capture and send minidumps from a
separate crash reporter process.

It spawns the current executable again with an environment variable that makes
it start in crash reporter mode. In this mode it waits for a minidump
notification from the app process and passes the minidump file to a user
defined closure.

```toml
[dependencies]
minidumper-child = "0.6"
```

```rust
use std::path::Path;

use minidumper_child::{CrashContext, MessageSender, MinidumperChild};

fn main() {
    // Everything before here runs in both the app and crash reporter processes
    let _guard = MinidumperChild::new()
        .on_crash(|crash_context: &CrashContext, sender: &MessageSender| {
            // Runs in the app process when it crashes, before the minidump
            // is requested. Only signal-safe work is allowed here.
            sender.send_message(1, b"crashed").ok();
        })
        .on_message(|kind: u32, buffer: Vec<u8>| {
            // Runs in the crash reporter process for each message from the app
        })
        .on_minidump(|buffer: Vec<u8>, path: &Path| {
            // Runs in the crash reporter process with the minidump file
        })
        .spawn();
    // Everything after here only runs in the app process

    App::run();

    // This will cause on_minidump to be called in the crash reporter process
    #[allow(deref_nullptr)]
    unsafe {
        *std::ptr::null_mut() = true;
    }
}
```

## Hooks

`spawn` panics unless you set at least one of `on_minidump` or `on_message`.

### `on_minidump`

Runs in the crash reporter process with the minidump bytes and the path where
they were written.

### `on_message`

Runs in the crash reporter process with each message sent from the app
process. While the app runs, send messages with `send_message` on the
`ClientHandle` that `spawn` returns.

### `on_crash`

Runs in the app process at the moment of the crash, before the minidump is
requested. It gets the `CrashContext`, which names the crashing thread, and a
`MessageSender`. Messages sent from it arrive at `on_message` in the crash
reporter before `on_minidump` runs.

It runs inside the crash handler, so only signal-safe work is allowed there.
Do not allocate, take locks or print. Send fixed size buffers from the stack.
See the `on_crash` documentation for the limits on each platform.

### `on_process`

Runs once in the app process before the crash reporter is spawned. It gets the
`Command` so you can change the arguments, environment or standard streams of
the crash reporter process.

## Options

| Method | Default | Purpose |
| --- | --- | --- |
| `with_crashes_dir` | `<temp dir>/Crashes` | Directory where minidump files are written |
| `with_server_stale_timeout` | 5 s | How long the crash reporter waits for a message before it exits |
| `with_client_connect_timeout` | 3 s | How long the app waits to connect to the crash reporter |
| `with_server_env_var` | `_CRASH_REPORTER_SERVER` | Environment variable that marks the crash reporter process |

Call `is_crash_reporter_process` to find out which process you are in before
`spawn`.
