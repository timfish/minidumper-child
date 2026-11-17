use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{Error, OwnedSocketName};

/// Tracks calls into the user callbacks, so the app watcher can tell when
/// the server has handled every message.
#[derive(Default)]
struct Activity {
    busy: AtomicBool,
    calls: AtomicU64,
}

impl Activity {
    fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        self.busy.store(true, Ordering::SeqCst);
        let result = f();
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    /// Returns when no call has run for `quiet`.
    fn wait_idle(&self, quiet: Duration) {
        loop {
            let calls = self.calls.load(Ordering::SeqCst);
            std::thread::sleep(quiet);
            if !self.busy.load(Ordering::SeqCst) && self.calls.load(Ordering::SeqCst) == calls {
                return;
            }
        }
    }
}

struct Handler<Minidump, Message>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    crashes_dir: PathBuf,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
    activity: Arc<Activity>,
}

impl<Minidump, Message> Handler<Minidump, Message>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    pub fn new(
        crashes_dir: PathBuf,
        on_minidump: Option<Minidump>,
        on_message: Option<Message>,
        activity: Arc<Activity>,
    ) -> Self {
        Handler {
            crashes_dir,
            on_minidump,
            on_message,
            activity,
        }
    }
}

impl<Minidump, Message> ServerHandler for Handler<Minidump, Message>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    /// Called when a crash has been received and a backing file needs to be
    /// created to store it.
    fn create_minidump_file(&self) -> Result<(File, PathBuf), io::Error> {
        fs::create_dir_all(&self.crashes_dir)?;
        let file_name = format!("{}.dmp", uuid::Uuid::new_v4());
        let path = self.crashes_dir.join(file_name);
        Ok((File::create(&path)?, path))
    }

    /// Called when a crash has been fully written as a minidump to the provided
    /// file. Also returns the full heap buffer as well.
    fn on_minidump_created(&self, result: Result<MinidumpBinary, minidumper::Error>) -> LoopAction {
        if let Ok(mut minidump) = result {
            if let Some(buffer) = minidump.contents.or_else(|| {
                minidump.file.flush().ok().and_then(|_| {
                    let mut buf = Vec::new();
                    File::open(&minidump.path)
                        .unwrap()
                        .read_to_end(&mut buf)
                        .map(|_| buf)
                        .ok()
                })
            }) {
                if let Some(on_minidump) = &self.on_minidump {
                    self.activity.run(|| on_minidump(buffer, &minidump.path));
                }
            }

            fs::remove_file(minidump.path).ok();
        }

        // Tells the server to exit, which will in turn exit the process
        LoopAction::Exit
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        if let Some(on_message) = &self.on_message {
            self.activity.run(|| on_message(kind, buffer));
        }
    }

    fn on_client_disconnected(&self, _num_clients: usize) -> minidumper::LoopAction {
        LoopAction::Exit
    }
}

/// The app process, watched from the crash reporter.
#[cfg(unix)]
struct AppProcess(u32);

#[cfg(unix)]
impl AppProcess {
    fn open(pid: u32) -> Option<Self> {
        Some(Self(pid))
    }

    /// Returns `true` when the app has exited.
    fn wait(self) -> bool {
        // When the app exits, this process gets a new parent. This also
        // fires at once if `on_process` makes the crash reporter a child
        // of some other process.
        while std::os::unix::process::parent_id() == self.0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    }
}

/// The app process, watched from the crash reporter.
#[cfg(windows)]
struct AppProcess(usize);

#[cfg(windows)]
impl AppProcess {
    fn open(pid: u32) -> Option<Self> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};

        // The open handle stops Windows from giving the pid to a new process
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        // A raw pointer is not `Send`
        (!handle.is_null()).then_some(Self(handle as usize))
    }

    /// Returns `true` when the app has exited.
    fn wait(self) -> bool {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
            System::Threading::{WaitForSingleObject, INFINITE},
        };

        let handle = self.0 as HANDLE;
        unsafe {
            let result = WaitForSingleObject(handle, INFINITE);
            CloseHandle(handle);
            result == WAIT_OBJECT_0
        }
    }
}

/// Sets `shutdown` after the app process exits and the server has
/// handled the messages it sent. If the app cannot be watched, the
/// server exits only when the app closes its socket.
fn watch_app(app_pid: u32, activity: Arc<Activity>, shutdown: Arc<AtomicBool>) {
    let Some(app) = AppProcess::open(app_pid) else {
        return;
    };

    std::thread::spawn(move || {
        if !app.wait() {
            return;
        }
        // The server reads one message per loop and stops as soon as
        // `shutdown` is set. Wait until it has read the messages the app
        // sent before it exited.
        activity.wait_idle(Duration::from_millis(500));
        shutdown.store(true, Ordering::Relaxed);
    });
}

/// Runs the crash reporter until the app closes its socket or a minidump
/// is written.
///
/// `stale_timeout` is not used, and the crash reporter does not watch the
/// app process. Use [`MinidumperChild`](crate::MinidumperChild) instead.
#[deprecated(note = "Use `MinidumperChild::spawn`. This module will become private")]
pub fn start<Minidump, Message>(
    socket_name: OwnedSocketName,
    crashes_dir: PathBuf,
    _stale_timeout: Duration,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
) -> Result<(), Error>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    run(socket_name, crashes_dir, None, on_minidump, on_message)
}

/// Runs the crash reporter until the app exits or a minidump is written.
pub(crate) fn run<Minidump, Message>(
    socket_name: OwnedSocketName,
    crashes_dir: PathBuf,
    app_pid: Option<u32>,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
) -> Result<(), Error>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    Server::with_name(socket_name.as_ref())
        .map_err(|err| Error::CreateServer {
            source: err,
            socket_name,
        })
        .and_then(|mut server| {
            let activity = Arc::new(Activity::default());
            let handler = Box::new(Handler::new(
                crashes_dir,
                on_minidump,
                on_message,
                activity.clone(),
            ));
            let shutdown = Arc::new(AtomicBool::new(false));

            if let Some(app_pid) = app_pid {
                watch_app(app_pid, activity, shutdown.clone());
            }

            // No stale timeout. The app can stop for any length of time,
            // for example when the OS suspends it, and must still be able
            // to send a crash.
            server
                .run(handler, &shutdown, None)
                .map_err(Error::RunServer)
        })
}
