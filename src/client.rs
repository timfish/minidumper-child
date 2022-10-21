use crate::Error;
use crash_handler::{make_crash_event, CrashContext, CrashEventResult, CrashHandler};
use minidumper::Client;
use std::{sync::Arc, time::Duration};

pub fn start(
    socket_name: &str,
    connect_timeout: u64,
    #[allow(unused_variables)] server_pid: u32,
    server_poll: u64,
) -> Result<(Arc<Client>, CrashHandler), Error> {
    let mut wait_time = 0;

    // Loop until we have a client or return error if connect_timeout is reached
    let client = loop {
        match minidumper::Client::with_name(socket_name).map(Arc::new) {
            Ok(client) => break client,
            Err(e) => {
                if wait_time < connect_timeout {
                    std::thread::sleep(Duration::from_millis(50));
                    wait_time += 50;
                } else {
                    return Err(Error::from(e));
                }
            }
        }
    };

    // Start a thread that pings the server so that it doesn't timeout and exit
    std::thread::spawn({
        let client = client.clone();
        move || loop {
            std::thread::sleep(Duration::from_millis(server_poll));

            if client.ping().is_err() {
                break;
            }
        }
    });

    let handler = CrashHandler::attach(unsafe {
        let client = client.clone();
        make_crash_event(move |crash_context: &CrashContext| {
            client.ping().ok();
            CrashEventResult::Handled(client.request_dump(crash_context).is_ok())
        })
    })?;

    // On linux we can explicitly allow only the server process to inspect the
    // process we are monitoring (this one) for crashes
    #[cfg(target_os = "linux")]
    handler.set_ptracer(Some(server_pid));

    Ok((client, handler))
}
