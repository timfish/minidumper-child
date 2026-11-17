use crate::{Error, MessageSender, OnCrash, OnRestart, OwnedSocketName};
use crash_handler::{make_crash_event, CrashContext, CrashEventResult, CrashHandler};
use minidumper::Client;
use std::{
    process::{Child, Command},
    ptr,
    sync::{
        atomic::{AtomicPtr, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Duration,
};

/// The connection to the current crash reporter process.
pub(crate) struct Connection {
    // Null when no crash reporter is running.
    current: AtomicPtr<Client>,
    // Owns every client made so far. The crash handler can hold a
    // reference to a replaced client, so none is freed before this is.
    clients: Mutex<Vec<Arc<Client>>>,
}

impl Connection {
    pub(crate) fn current(&self) -> Option<&Client> {
        // SAFETY: The pointer is null or points into `self.clients`,
        // which lives as long as `self`.
        unsafe { self.current.load(Ordering::Acquire).as_ref() }
    }

    fn set(&self, client: Option<Client>) {
        let ptr = match client {
            Some(client) => {
                let client = Arc::new(client);
                let ptr = Arc::as_ptr(&client).cast_mut();
                lock(&self.clients).push(client);
                ptr
            }
            None => ptr::null_mut(),
        };
        self.current.store(ptr, Ordering::Release);
    }
}

/// Holds the crash handler. The ping thread stops when it is taken.
pub(crate) type SharedHandler = Arc<Mutex<Option<CrashHandler>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn connect(socket_name: OwnedSocketName, connect_timeout: Duration) -> Result<Client, Error> {
    let mut wait_time = Duration::ZERO;

    // Loop until we have a client or return error if connect_timeout is reached
    loop {
        match Client::with_name(socket_name.as_ref()) {
            Ok(client) => return Ok(client),
            Err(e) => {
                if wait_time < connect_timeout {
                    let wait = Duration::from_millis(50);
                    std::thread::sleep(wait);
                    wait_time += wait;
                } else {
                    return Err(Error::CreateClient {
                        source: e,
                        socket_name,
                    });
                }
            }
        }
    }
}

fn spawn_server(
    command: &mut Command,
    server_env: &str,
    connect_timeout: Duration,
) -> Result<(Client, Child), Error> {
    // We use a unique socket name because we don't share the crash reporter
    // processes between different instances of the app.
    let socket_name = OwnedSocketName::new(&std::env::temp_dir(), uuid::Uuid::new_v4());

    // Always set this last, so an accidental `env_clear()` doesn't remove it.
    command.env(server_env, socket_name.as_os_string());
    let mut child = command.spawn().map_err(Error::SpawnProcess)?;

    match connect(socket_name, connect_timeout) {
        Ok(client) => Ok((client, child)),
        Err(e) => {
            // With no client the server never exits by itself
            child.kill().ok();
            child.wait().ok();
            Err(e)
        }
    }
}

pub(crate) fn start(
    mut command: Command,
    server_env: String,
    connect_timeout: Duration,
    server_poll: Duration,
    on_crash: Option<OnCrash>,
    on_restart: Option<OnRestart>,
) -> Result<(Arc<Connection>, SharedHandler), Error> {
    let (client, mut child) = spawn_server(&mut command, &server_env, connect_timeout)?;

    let connection = Arc::new(Connection {
        current: AtomicPtr::new(ptr::null_mut()),
        clients: Mutex::new(Vec::new()),
    });
    connection.set(Some(client));

    let handler = CrashHandler::attach(unsafe {
        let connection = connection.clone();
        make_crash_event(move |crash_context: &CrashContext| {
            let Some(client) = connection.current() else {
                return CrashEventResult::Handled(false);
            };
            if let Some(on_crash) = &on_crash {
                on_crash(crash_context, &MessageSender(client));
            }
            // The server answers a ping only after it has handled every
            // earlier message on this connection, so messages sent by
            // `on_crash` reach `on_message` before the dump is written.
            client.ping().ok();
            let dumped = match client.request_dump(crash_context) {
                Ok(()) => true,
                // The server has sent its ack, so the dump is written.
                // crash-context reads the ack into a buffer with no room
                // for the trailer the kernel adds, so the read fails.
                #[cfg(target_os = "macos")]
                Err(minidumper::Error::PortError(crash_context::ipc::Error::Message(
                    mach2::message::MACH_RCV_TOO_LARGE,
                ))) => true,
                Err(_) => false,
            };
            // Exit here so no signal kills the process. Otherwise the
            // kernel raises EXC_CRASH and Apple's crash reporter shows
            // the "quit unexpectedly" dialog. `_exit` is signal-safe.
            #[cfg(target_os = "macos")]
            if dumped {
                libc::_exit(1);
            }
            CrashEventResult::Handled(dumped)
        })
    })
    .map_err(Error::AttachCrashHandler)?;

    // On linux we can explicitly allow only the server process to inspect the
    // process we are monitoring (this one) for crashes
    #[cfg(target_os = "linux")]
    handler.set_ptracer(Some(child.id()));

    let handler: SharedHandler = Arc::new(Mutex::new(Some(handler)));

    // Start a thread that pings the server so that it doesn't timeout and
    // exit. If the server has gone, start a new one.
    std::thread::spawn({
        let connection = connection.clone();
        let handler = handler.clone();
        move || loop {
            std::thread::sleep(server_poll);

            if lock(&handler).is_none() {
                return;
            }

            if connection.current().is_some_and(|c| c.ping().is_ok()) {
                continue;
            }

            // The server times out if this process does not run for a
            // while, for example when the OS suspends it.
            child.kill().ok();
            child.wait().ok();

            let new_server = spawn_server(&mut command, &server_env, connect_timeout).and_then(
                |(client, new_child)| match client.ping() {
                    Ok(()) => Ok((client, new_child)),
                    Err(e) => Err(Error::SendMessage(e)),
                },
            );

            {
                let handler = lock(&handler);
                let Some(_handler) = &*handler else {
                    return;
                };

                match new_server {
                    Ok((client, new_child)) => {
                        #[cfg(target_os = "linux")]
                        _handler.set_ptracer(Some(new_child.id()));
                        connection.set(Some(client));
                        child = new_child;
                    }
                    Err(_) => {
                        // A server that fails at start would fail again
                        connection.set(None);
                        return;
                    }
                }
            }

            // Runs without the lock, so the callback can drop the
            // `ClientHandle` without a deadlock.
            if let (Some(on_restart), Some(client)) = (&on_restart, connection.current()) {
                on_restart(&MessageSender(client));
            }
        }
    });

    Ok((connection, handler))
}
