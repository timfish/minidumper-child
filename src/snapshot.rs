use std::{
    io,
    sync::{
        atomic::{AtomicI32, Ordering},
        Weak,
    },
};

/// The write end of the self-pipe, read by the thread spawned in
/// [`bind_signal`]. Stored in a static so the signal handler can reach it.
static PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// Only writes to the pipe, which is async-signal-safe. The dump is requested
/// from the reader thread where we're free to allocate, lock and block.
unsafe extern "C" fn on_signal(_signal: libc::c_int) {
    let fd = PIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 0u8;
        unsafe { libc::write(fd, std::ptr::from_ref(&byte).cast(), 1) };
    }
}

/// Installs a handler for `signal` which captures a minidump of this process
/// without exiting it.
pub fn bind_signal(
    signal: i32,
    handler: Weak<crash_handler::CrashHandler>,
    conn: Weak<crate::client::ServerConnection>,
) -> Result<(), io::Error> {
    let mut fds = [-1i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let [read_fd, write_fd] = fds;

    for fd in fds {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }

    PIPE_WRITE_FD.store(write_fd, Ordering::SeqCst);

    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    std::thread::spawn(move || loop {
        let mut buf = [0u8; 1];
        let read = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), 1) };
        if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if read <= 0 {
            break;
        }

        let (Some(handler), Some(conn)) = (handler.upgrade(), conn.upgrade()) else {
            break;
        };
        crate::client::capture_minidump(&handler, &conn);
    });

    Ok(())
}
