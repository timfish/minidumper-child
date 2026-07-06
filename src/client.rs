use crate::{Error, OwnedSocketName};
use crash_handler::{make_crash_event, CrashContext, CrashEventResult, CrashHandler};
use minidumper::Client;
use std::{sync::Arc, time::Duration};

pub fn start(
    socket_name: OwnedSocketName,
    connect_timeout: Duration,
    #[allow(unused_variables)] server_pid: u32,
    server_poll: Duration,
) -> Result<(Arc<Client>, CrashHandler), Error> {
    let mut wait_time = Duration::ZERO;

    // Loop until we have a client or return error if connect_timeout is reached
    let client = loop {
        match minidumper::Client::with_name(socket_name.as_ref()).map(Arc::new) {
            Ok(client) => break client,
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
    };

    // Start a thread that pings the server so that it doesn't timeout and exit
    std::thread::spawn({
        let client = client.clone();
        move || loop {
            std::thread::sleep(server_poll);

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
    })
    .map_err(Error::AttachCrashHandler)?;

    // On linux we can explicitly allow only the server process to inspect the
    // process we are monitoring (this one) for crashes
    #[cfg(target_os = "linux")]
    handler.set_ptracer(Some(server_pid));

    Ok((client, handler))
}
