use std::{
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::Duration,
};

pub mod client;
pub mod server;

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

pub type OnProcess = Box<dyn FnOnce(&mut process::Command) + Send + Sync + 'static>;
pub type OnMinidump = Box<dyn Fn(Vec<u8>, &Path) + Send + Sync + 'static>;
pub type OnMessage = Box<dyn Fn(u32, Vec<u8>) + Send + Sync + 'static>;

pub struct MinidumperChild {
    crashes_dir: PathBuf,
    server_stale_timeout: Duration,
    client_connect_timeout: Duration,
    server_env: String,
    on_process: Option<OnProcess>,
    on_minidump: Option<OnMinidump>,
    on_message: Option<OnMessage>,
}

impl Default for MinidumperChild {
    fn default() -> Self {
        Self {
            crashes_dir: std::env::temp_dir().join("Crashes"),
            server_stale_timeout: Duration::from_millis(5000),
            client_connect_timeout: Duration::from_millis(3000),
            server_env: "_CRASH_REPORTER_SERVER".to_owned(),
            on_process: None,
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

    /// Returns whether the current process is the crash reporting server process.
    pub fn is_crash_reporter_process(&self) -> bool {
        std::env::var_os(&self.server_env).is_some()
    }

    /// Configures a callback which can be used to modify the server process.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn on_process<F>(mut self, on_process: F) -> Self
    where
        F: FnOnce(&mut process::Command) + Send + Sync + 'static,
    {
        self.on_process = Some(Box::new(on_process));
        self
    }

    /// Configures a callback which is invoked with the generated minidump on crash.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn on_minidump<F>(mut self, on_minidump: F) -> Self
    where
        F: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    {
        self.on_minidump = Some(Box::new(on_minidump));
        self
    }

    /// Configures a callback which is invoked with messages from the app process.
    ///
    /// A message may be sent using [`ClientHandle::send_message`].
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn on_message<F>(mut self, on_message: F) -> Self
    where
        F: Fn(u32, Vec<u8>) + Send + Sync + 'static,
    {
        self.on_message = Some(Box::new(on_message));
        self
    }

    /// Configures the directory where crash dumps will be stored.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_crashes_dir(mut self, crashes_dir: PathBuf) -> Self {
        self.crashes_dir = crashes_dir;
        self
    }

    /// Configures the server stale timeout.
    ///
    /// The server expects periodic messages from the client process, if it does not receive
    /// a message within `timeout`, it considers the connection broken and detach.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_server_stale_timeout(mut self, timeout: Duration) -> Self {
        self.server_stale_timeout = timeout;
        self
    }

    /// Configures the client connect timeout.
    ///
    /// This is the amount of time the client will wait to establish a connection to the server.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_client_connect_timeout(mut self, timeout: Duration) -> Self {
        self.client_connect_timeout = timeout;
        self
    }

    /// Configures the name of an environment variable which is passed to the server process.
    ///
    /// This defaults to `_CRASH_REPORTER_SERVER`.
    #[must_use = "You should call spawn() or the crash reporter won't be enabled"]
    pub fn with_server_env_var(mut self, name: String) -> Self {
        self.server_env = name;
        self
    }

    #[must_use = "The return value of spawn() should not be dropped until the program exits"]
    pub fn spawn(self) -> Result<ClientHandle, Error> {
        if self.on_minidump.is_none() && self.on_message.is_none() {
            panic!("You should set one of 'on_minidump' or 'on_message'");
        }

        if let Ok(socket_name) = std::env::var(&self.server_env) {
            let socket_name = minidumper::SocketName::path(&socket_name);

            server::start(
                socket_name,
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
                    let mut process = process::Command::new(current_exe);
                    if let Some(on_process) = self.on_process {
                        on_process(&mut process);
                    }
                    // Always set this last, so an accidental `env_clear()` doesn't remove it.
                    process.env(self.server_env, &socket_name);
                    process.spawn()
                })
                .map_err(Error::from)
                .and_then(|server_process| {
                    client::start(
                        minidumper::SocketName::path(&socket_name),
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
