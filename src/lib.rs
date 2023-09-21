use std::{
    path::{Path, PathBuf},
    process,
    sync::Arc,
};

mod client;
mod server;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    CrashHandler(#[from] crash_handler::Error),
    #[error(transparent)]
    Minidumper(#[from] minidumper::Error),
}

pub struct ClientHandle {
    client: Arc<minidumper::Client>,
    _handler: crash_handler::CrashHandler,
    _child: process::Child,
}

impl ClientHandle {
    pub fn send_message(&self, kind: u32, buf: impl AsRef<[u8]>) -> Result<(), Error> {
        self.client.send_message(kind, buf).map_err(Error::from)
    }
}

pub type OnMinidump = Box<dyn Fn(Vec<u8>, &Path) + Send + Sync + 'static>;
pub type OnMessage = Box<dyn Fn(u32, Vec<u8>) + Send + Sync + 'static>;

pub struct MinidumperChild {
    crashes_dir: PathBuf,
    server_stale_timeout: u64,
    client_connect_timeout: u64,
    server_arg: String,
    on_minidump: Option<OnMinidump>,
    on_message: Option<OnMessage>,
}

impl Default for MinidumperChild {
    fn default() -> Self {
        Self {
            crashes_dir: std::env::temp_dir().join("Crashes"),
            server_stale_timeout: 5000,
            client_connect_timeout: 3000,
            server_arg: "--crash-reporter-server".to_string(),
            on_minidump: None,
            on_message: None,
        }
    }
}

impl MinidumperChild {
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_crash_reporter_process(&self) -> bool {
        std::env::args().any(|arg| arg.starts_with(&self.server_arg))
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn on_minidump<F>(mut self, on_minidump: F) -> Self
    where
        F: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    {
        self.on_minidump = Some(Box::new(on_minidump));
        self
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn on_message<F>(mut self, on_message: F) -> Self
    where
        F: Fn(u32, Vec<u8>) + Send + Sync + 'static,
    {
        self.on_message = Some(Box::new(on_message));
        self
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_crashes_dir(mut self, crashes_dir: PathBuf) -> Self {
        self.crashes_dir = crashes_dir;
        self
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_server_stale_timeout(mut self, server_stale_timeout: u64) -> Self {
        self.server_stale_timeout = server_stale_timeout;
        self
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_client_connect_timeout(mut self, client_connect_timeout: u64) -> Self {
        self.client_connect_timeout = client_connect_timeout;
        self
    }

    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_server_arg(mut self, server_arg: String) -> Self {
        self.server_arg = server_arg;
        self
    }

    #[must_use = "The return value of spawn() should not be dropped until the program exits"]
    pub fn spawn(self) -> Result<ClientHandle, Error> {
        if self.on_minidump.is_none() && self.on_message.is_none() {
            panic!("You should set one of 'on_minidump' or 'on_message'");
        }

        let server_socket = std::env::args()
            .find(|arg| arg.starts_with(&self.server_arg))
            .and_then(|arg| arg.split('=').last().map(|arg| arg.to_string()));

        if let Some(socket_name) = server_socket {
            server::start(
                &socket_name,
                self.crashes_dir,
                self.server_stale_timeout,
                self.on_minidump,
                self.on_message,
            )?;

            // We force exit so that the app code after here does not run in the
            // crash reporter process.
            std::process::exit(0);
        } else {
            // We use a unique socket name because we don't share the crash reporter
            // processes between different instances of the app.
            let socket_name = make_socket_name(uuid::Uuid::new_v4());

            std::env::current_exe()
                .and_then(|current_exe| {
                    process::Command::new(current_exe)
                        .arg(format!("{}={}", &self.server_arg, socket_name))
                        .spawn()
                })
                .map_err(Error::from)
                .and_then(|server_process| {
                    client::start(
                        &socket_name,
                        self.client_connect_timeout,
                        server_process.id(),
                        self.server_stale_timeout / 2,
                    )
                    .map(|(client, handler)| ClientHandle {
                        client,
                        _handler: handler,
                        _child: server_process,
                    })
                })
        }
    }
}

pub fn make_socket_name(session_id: uuid::Uuid) -> String {
    if cfg!(any(target_os = "linux", target_os = "android")) {
        format!("temp-socket-{}", session_id.simple())
    } else {
        // For platforms without abstract uds, put the pipe in the
        // temporary directory so that the OS can clean it up, rather than
        // polluting the cwd due to annoying file deletion problems,
        // particularly on Windows
        let mut td = std::env::temp_dir();
        td.push(format!("temp-socket-{}", session_id.simple()));
        td.to_string_lossy().to_string()
    }
}
