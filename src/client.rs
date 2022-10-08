use crate::duplex::Duplex;
use crate::{errors::ShamanError, SIZE};
use fehler::throws;
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::os::unix::net::UnixStream;
use std::os::unix::prelude::AsRawFd;
use std::{
    fs::File,
    os::unix::{io::FromRawFd, prelude::RawFd},
    path::Path,
};

pub struct ShamanClient {
    _stream: UnixStream,
    duplex: Duplex,
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

        let len = usize::from_ne_bytes(len);
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

        Self {
            _stream: stream,
            duplex: Duplex::new(tx, rx),
        }
    }

    #[throws(ShamanError)]
    pub fn recv(&mut self) -> Option<Vec<u8>> {
        self.duplex.rx.block_until_readable()?;

        self.try_recv()?
    }

    #[throws(ShamanError)]
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.duplex.recv()?
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, data: &[u8]) {
        self.duplex.send(data)?;
    }

    /// Get the RawFd to get the notification when the send side is not full.
    /// Useful for the epoll/mio based application
    pub fn send_notify_fd(&self) -> RawFd {
        self.duplex.tx.full_signal().as_raw_fd()
    }

    /// Get the RawFd to get the notification when the recv side is not empty.
    /// Useful for the epoll/mio based application
    pub fn recv_notify_fd(&self) -> RawFd {
        self.duplex.rx.empty_signal().as_raw_fd()
    }
}
