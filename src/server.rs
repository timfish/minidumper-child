use minidumper::{LoopAction, MinidumpBinary, Server, ServerHandler};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

use crate::Error;

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

pub fn start<Minidump, Message>(
    socket_name: &str,
    crashes_dir: PathBuf,
    stale_timeout: u64,
    on_minidump: Option<Minidump>,
    on_message: Option<Message>,
) -> Result<(), Error>
where
    Minidump: Fn(Vec<u8>, &Path) + Send + Sync + 'static,
    Message: Fn(u32, Vec<u8>) + Send + Sync + 'static,
{
    Server::with_name(socket_name)
        .map_err(Error::from)
        .and_then(|mut server| {
            let handler = Box::new(Handler::new(crashes_dir, on_minidump, on_message));
            let shutdown = AtomicBool::new(false);
            let stale_timeout = Some(Duration::from_millis(stale_timeout));

            server
                .run(handler, &shutdown, stale_timeout)
                .map_err(Error::from)
        })
}
