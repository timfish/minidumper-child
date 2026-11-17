use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{Error, OwnedSocketName};

/// Holds the process id of the app, so the crash reporter can exit when
/// the app does.
pub(crate) const APP_PID_ENV: &str = "_MINIDUMPER_CHILD_APP_PID";

struct Handler<Minidump, Message>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    crashes_dir: PathBuf,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
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
    ) -> Self {
        Handler {
            crashes_dir,
            on_minidump,
            on_message,
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
                    on_minidump(buffer, &minidump.path)
                }
            }

            fs::remove_file(minidump.path).ok();
        }

        // Tells the server to exit, which will in turn exit the process
        LoopAction::Exit
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        if let Some(on_message) = &self.on_message {
            on_message(kind, buffer);
        }
    }

    fn on_client_disconnected(&self, _num_clients: usize) -> minidumper::LoopAction {
        LoopAction::Exit
    }
}

/// Sets `shutdown` when the app process exits.
#[cfg(unix)]
fn watch_app(app_pid: u32, shutdown: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        // When the app exits, this process gets a new parent
        while std::os::unix::process::parent_id() == app_pid {
            std::thread::sleep(Duration::from_millis(100));
        }
        shutdown.store(true, Ordering::Relaxed);
    });
}

/// Sets `shutdown` when the app process exits.
#[cfg(windows)]
fn watch_app(app_pid: u32, shutdown: Arc<AtomicBool>) {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE},
    };

    // The open handle stops Windows from giving the pid to a new process
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, app_pid) };
    if handle.is_null() {
        // The server still exits when the app closes its socket
        return;
    }

    // A raw pointer is not `Send`
    let handle = handle as usize;
    std::thread::spawn(move || {
        unsafe {
            WaitForSingleObject(handle as HANDLE, INFINITE);
            CloseHandle(handle as HANDLE);
        }
        shutdown.store(true, Ordering::Relaxed);
    });
}

/// Runs the crash reporter until the app exits or a minidump is written.
///
/// `stale_timeout` is not used. The crash reporter watches the app
/// process instead.
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
    Server::with_name(socket_name.as_ref())
        .map_err(|err| Error::CreateServer {
            source: err,
            socket_name,
        })
        .and_then(|mut server| {
            let handler = Box::new(Handler::new(crashes_dir, on_minidump, on_message));
            let shutdown = Arc::new(AtomicBool::new(false));

            if let Some(app_pid) = std::env::var(APP_PID_ENV)
                .ok()
                .and_then(|pid| pid.parse().ok())
            {
                watch_app(app_pid, shutdown.clone());
            }

            // No stale timeout. The app can stop for any length of time,
            // for example when the OS suspends it, and must still be able
            // to send a crash.
            server
                .run(handler, &shutdown, None)
                .map_err(Error::RunServer)
        })
}
