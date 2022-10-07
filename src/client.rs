use crate::duplex::Duplex;
use crate::{errors::ShamanError, SIZE};
use crate::{Request, Response};
use fehler::throws;
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::os::unix::net::UnixStream;
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

        Self {
            _stream: stream,
            duplex: Duplex::new(tx, rx),
        }
    }

    #[throws(ShamanError)]
    pub fn recv(&mut self) -> Option<Response> {
        self.duplex.rx.block_until_readable()?;

        self.duplex.recv()?;

        match self.duplex.decode()? {
            None => None,
            Some(data) => {
                let resp: Response = bincode::deserialize(&data[SIZE..])?;
                Some(resp)
            }
        }
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, id: u64, method: &str, params: &[u8]) {
        let req = Request {
            id,
            method: method.into(),
            params: params.into(),
        };
        let serialized = bincode::serialize(&req)?;
        self.duplex.send(&serialized)?;
    }
}
