use crate::{errors::ShamanError, SIZE};
use fehler::throws;
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::{
    fs::File,
    os::unix::{io::FromRawFd, prelude::RawFd},
    path::Path,
};

pub struct ShamanClient {
    stream: std::os::unix::net::UnixStream,
    tx: IPCSender<u8>,
    rx: IPCReceiver<u8>,
}

impl ShamanClient {
    #[throws(ShamanError)]
    pub fn new<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        let stream = std::os::unix::net::UnixStream::connect(path)?;

        let mut len = [0; SIZE];
        let mut fds: [RawFd; 6] = [-1; 6];
        if let Err(e) = stream.recv_with_fd(&mut len, &mut fds) {
            panic!("Cannot recv fds: {}", e);
        };

        let len = usize::from_le_bytes(len);
        let tx = unsafe {
            IPCSender::open(
                len,
                File::from_raw_fd(fds[0]),
                File::from_raw_fd(fds[1]),
                File::from_raw_fd(fds[2]),
            )?
        };
        let rx = unsafe {
            IPCReceiver::open(
                len,
                File::from_raw_fd(fds[3]),
                File::from_raw_fd(fds[4]),
                File::from_raw_fd(fds[5]),
            )?
        };

        Self { stream, tx, rx }
    }
}
