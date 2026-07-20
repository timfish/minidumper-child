use crate::{Error, OwnedSocketName};
use crash_handler::{make_crash_event, CrashContext, CrashEventResult, CrashHandler};
use minidumper::Client;
use std::{
    sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError},
    time::Duration,
};

/// The connection to the crash reporter server.
///
/// `ping` and `request_dump` both read a response from the socket, so all
/// request/response pairs are serialized through `io_lock` to stop one
/// consuming the other's response.
///
/// The server closes the connection after serving a dump, so the connection
/// can be re-established with `reconnect`.
pub struct ServerConnection {
    socket_name: OwnedSocketName,
    reconnect_timeout: Duration,
    client: RwLock<Arc<Client>>,
    io_lock: Mutex<()>,
}

impl ServerConnection {
    fn connect(
        socket_name: &OwnedSocketName,
        timeout: Duration,
    ) -> Result<Arc<Client>, minidumper::Error> {
        let mut wait_time = Duration::ZERO;

        loop {
            match Client::with_name(socket_name.as_ref()).map(Arc::new) {
                Ok(client) => break Ok(client),
                Err(e) => {
                    if wait_time < timeout {
                        let wait = Duration::from_millis(50);
                        std::thread::sleep(wait);
                        wait_time += wait;
                    } else {
                        break Err(e);
                    }
                }
            }
        }
    }

    fn current_client(&self) -> Arc<Client> {
        self.client
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn send_message(&self, kind: u32, buf: impl AsRef<[u8]>) -> Result<(), minidumper::Error> {
        // Sends don't read a response so they don't need the io_lock
        self.current_client().send_message(kind, buf)
    }

    fn ping(&self) -> Result<(), minidumper::Error> {
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.current_client().ping()
    }

    fn request_dump(&self, crash_context: &CrashContext) -> bool {
        // During a real crash this runs in a signal handler, so don't risk
        // blocking on the lock forever; a dump without the lock is better
        // than no dump at all.
        let _guard = try_lock_briefly(&self.io_lock);

        self.current_client().request_dump(crash_context).is_ok()
    }

    fn reconnect(&self) -> bool {
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());

        match Self::connect(&self.socket_name, self.reconnect_timeout) {
            Ok(client) => {
                *self.client.write().unwrap_or_else(|e| e.into_inner()) = client;
                true
            }
            Err(_) => false,
        }
    }
}

/// Captures a minidump of the current process via the attached crash handler
/// and restores the server connection afterwards. Unlike a real crash, the
/// process continues afterwards.
pub(crate) fn capture_minidump(handler: &CrashHandler, conn: &ServerConnection) -> bool {
    if !simulate_crash(handler) {
        return false;
    }

    // The server closes the connection after serving a dump, so reconnect
    // for subsequent dumps and pings
    conn.reconnect()
}

/// Invokes the attached crash event with a synthesized crash context for the
/// calling thread.
fn simulate_crash(handler: &CrashHandler) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    return matches!(
        handler.simulate_signal(crash_handler::Signal::Trap as u32),
        CrashEventResult::Handled(true)
    );

    #[cfg(target_os = "windows")]
    return matches!(
        handler.simulate_exception(None),
        CrashEventResult::Handled(true)
    );

    #[cfg(target_os = "macos")]
    return handler.simulate_exception(None);
}

pub fn start(
    socket_name: OwnedSocketName,
    connect_timeout: Duration,
    #[allow(unused_variables)] server_pid: u32,
    server_poll: Duration,
) -> Result<(Arc<ServerConnection>, CrashHandler), Error> {
    let client = ServerConnection::connect(&socket_name, connect_timeout).map_err(|e| {
        Error::CreateClient {
            source: e,
            socket_name: socket_name.clone(),
        }
    })?;

    let conn = Arc::new(ServerConnection {
        socket_name,
        reconnect_timeout: connect_timeout,
        client: RwLock::new(client),
        io_lock: Mutex::new(()),
    });

    // Start a thread that pings the server so that it doesn't timeout and exit.
    // Errors are ignored because the connection is briefly down while it's
    // re-established after a dump; the thread exits when the ClientHandle and
    // therefore the last strong reference is dropped.
    std::thread::spawn({
        let conn = Arc::downgrade(&conn);
        move || loop {
            std::thread::sleep(server_poll);

            let Some(conn) = conn.upgrade() else {
                break;
            };

            conn.ping().ok();
        }
    });

    let handler = CrashHandler::attach(unsafe {
        let conn = conn.clone();
        make_crash_event(move |crash_context: &CrashContext| {
            CrashEventResult::Handled(conn.request_dump(crash_context))
        })
    })
    .map_err(Error::AttachCrashHandler)?;

    // On linux we can explicitly allow only the server process to inspect the
    // process we are monitoring (this one) for crashes
    #[cfg(target_os = "linux")]
    handler.set_ptracer(Some(server_pid));

    Ok((conn, handler))
}

/// Attempts to acquire the lock for up to ~2 seconds, which is ample time for
/// an in-flight ping to complete.
fn try_lock_briefly(lock: &Mutex<()>) -> Option<MutexGuard<'_, ()>> {
    for _ in 0..400 {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    None
}
