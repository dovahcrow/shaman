use crate::{
    duplex::{BufReceiver, BufSender},
    errors::ShamanError::{self, *},
    SIZE,
};
use fehler::{throw, throws};
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::{
    fs::File,
    os::unix::{
        io::FromRawFd,
        prelude::{AsRawFd, RawFd},
    },
    path::Path,
};
use tokio::{
    io::{unix::AsyncFd, Interest},
    net::UnixStream,
    select,
};

pub struct ShamanAsyncClient {
    stream: UnixStream,
    tx: BufSender,
    rx: BufReceiver,
    tx_notifier: AsyncFd<RawFd>,
    rx_notifier: AsyncFd<RawFd>,
}

impl ShamanAsyncClient {
    #[throws(ShamanError)]
    pub async fn new<P>(path: P, capacity: usize) -> Self
    where
        P: AsRef<Path>,
    {
        let stream = UnixStream::connect(path).await?;

        // send the wanted shmem channel capacity to server
        stream.writable().await?;
        stream.try_write(&capacity.to_ne_bytes())?;

        let mut len = [0; SIZE];
        let mut fds: [RawFd; 6] = [-1; 6];
        stream.readable().await?;
        let stdstream = stream.into_std()?; // todo: patch sendfd to support tokio
        stdstream.recv_with_fd(&mut len, &mut fds)?;
        let stream = UnixStream::from_std(stdstream)?;

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

        let tx = BufSender::new(tx);
        let rx = BufReceiver::new(rx);

        Self {
            stream,
            tx_notifier: AsyncFd::new(tx.notifier().as_raw_fd())?,
            tx,
            rx_notifier: AsyncFd::new(rx.notifier().as_raw_fd())?,
            rx,
        }
    }

    #[throws(ShamanError)]
    pub async fn recv(&mut self) -> Vec<u8> {
        loop {
            match self.try_recv() {
                Ok(v) => break v,
                Err(WouldBlock) => {
                    select! {
                        ready = self.stream.ready(Interest::READABLE) => {
                            if ready?.is_read_closed() {
                                throw!(ConnectionClosed)
                            }
                        }
                        guard = self.rx_notifier.readable() => {
                            guard?.clear_ready();
                        }
                    }
                }
                Err(e) => throw!(e),
            }
        }
    }

    // zero copy
    #[throws(ShamanError)]
    pub async fn recv_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&[u8]) -> R,
    {
        loop {
            match self.try_recv_with(f) {
                Ok(v) => break v,
                Err(WouldBlock) => {
                    select! {
                        ready = self.stream.ready(Interest::READABLE) => {
                            if ready?.is_read_closed() {
                                throw!(ConnectionClosed)
                            }
                        }
                        guard = self.rx_notifier.readable() => {
                            guard?.clear_ready();
                        }
                    }
                }

                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn try_recv(&mut self) -> Vec<u8> {
        self.rx.try_recv()?
    }

    // zero copy
    #[throws(ShamanError)]
    pub fn try_recv_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&[u8]) -> R,
    {
        self.rx.try_recv_with(f)?
    }

    #[throws(ShamanError)]
    pub async fn send(&mut self, data: &[u8]) {
        loop {
            match self.try_send(data) {
                Ok(_) => break,
                Err(WouldBlock) => {
                    select! {
                        ready = self.stream.ready(Interest::READABLE) => {
                            if ready?.is_read_closed() {
                                throw!(ConnectionClosed)
                            }
                        }
                        guard = self.tx_notifier.readable() => {
                            guard?.clear_ready();
                        }
                    }
                }
                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn try_send(&mut self, data: &[u8]) {
        self.tx.try_send(data)?
    }
}
