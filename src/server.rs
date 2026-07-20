use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{Error, OwnedSocketName};

/// State shared between the [`Handler`] and the idle watchdog thread.
#[derive(Default)]
struct SharedState {
    num_clients: AtomicUsize,
    /// After a dump is written, the server closes the client connection and
    /// the client reconnects. This marks that disconnect as expected so it
    /// doesn't shut down the server.
    dump_disconnect_expected: AtomicBool,
}

struct Handler<Minidump, Message>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    crashes_dir: PathBuf,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
    state: Arc<SharedState>,
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
        state: Arc<SharedState>,
    ) -> Self {
        Handler {
            crashes_dir,
            on_minidump,
            on_message,
            state,
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

        // The server closes the client connection after serving a dump and
        // the client then reconnects. On macOS the connection is closed
        // without on_client_disconnected being called, so the disconnect has
        // to be accounted for here instead.
        #[cfg(target_os = "macos")]
        self.state.num_clients.store(0, Ordering::SeqCst);
        #[cfg(not(target_os = "macos"))]
        self.state
            .dump_disconnect_expected
            .store(true, Ordering::SeqCst);

        // Keep serving so the app process can request further dumps (e.g.
        // snapshots via `ClientHandle::capture_minidump`)
        LoopAction::Continue
    }

    fn on_message(&self, kind: u32, buffer: Vec<u8>) {
        if let Some(on_message) = &self.on_message {
            on_message(kind, buffer);
        }
    }

    fn on_client_connected(&self, num_clients: usize) -> LoopAction {
        self.state.num_clients.store(num_clients, Ordering::SeqCst);
        LoopAction::Continue
    }

    fn on_client_disconnected(&self, num_clients: usize) -> minidumper::LoopAction {
        self.state.num_clients.store(num_clients, Ordering::SeqCst);

        if self
            .state
            .dump_disconnect_expected
            .swap(false, Ordering::SeqCst)
        {
            // The client reconnects after a dump; if it died in the meantime
            // (a real crash), the idle watchdog will shut the server down
            LoopAction::Continue
        } else {
            LoopAction::Exit
        }
    }
}

pub fn start<Minidump, Message>(
    socket_name: OwnedSocketName,
    crashes_dir: PathBuf,
    stale_timeout: Duration,
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
            let state = Arc::new(SharedState::default());
            let handler = Box::new(Handler::new(
                crashes_dir,
                on_minidump,
                on_message,
                state.clone(),
            ));
            let shutdown = Arc::new(AtomicBool::new(false));

            // Shuts the server down if there have been no clients for longer
            // than the stale timeout. This covers the app process dying after
            // a dump-induced disconnect, where on_client_disconnected has
            // already returned Continue expecting a reconnect that never comes.
            // It also covers the initial client never managing to connect.
            std::thread::spawn({
                let state = state.clone();
                let shutdown = shutdown.clone();
                let poll_interval = Duration::from_millis(100);
                move || {
                    let mut empty_for = Duration::ZERO;
                    loop {
                        std::thread::sleep(poll_interval);

                        if state.num_clients.load(Ordering::SeqCst) > 0 {
                            empty_for = Duration::ZERO;
                        } else {
                            empty_for += poll_interval;
                            if empty_for >= stale_timeout {
                                shutdown.store(true, Ordering::SeqCst);
                                break;
                            }
                        }
                    }
                }
            });

            server
                .run(handler, &shutdown, Some(stale_timeout))
                .map_err(Error::RunServer)
        })
}
