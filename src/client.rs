use crate::{
    duplex::{BufReceiver, BufSender},
    errors::ShamanError::{self, *},
    SIZE,
};
use fehler::{throw, throws};
use mio::{net::UnixStream, unix::SourceFd, Events, Interest, Poll, Token};
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Write},
    os::unix::{
        io::FromRawFd,
        prelude::{AsRawFd, RawFd},
    },
    path::Path,
    time::{Duration, Instant},
};

const SOCK: Token = Token(0);
const TX: Token = Token(1);
const RX: Token = Token(2);

pub struct ShamanClient {
    stream: UnixStream,
    tx: BufSender,
    rx: BufReceiver,
    poll: Poll,
    sendable: bool,
    receivable: bool,
    events: Events,
}

impl ShamanClient {
    #[throws(ShamanError)]
    pub fn new<P>(path: P, capacity: usize) -> Self
    where
        P: AsRef<Path>,
    {
        let mut stream = std::os::unix::net::UnixStream::connect(path)?;

        stream.write(&capacity.to_ne_bytes())?; // send the wanted shmem channel capacity to server

        let mut len = [0; SIZE];
        let mut fds: [RawFd; 6] = [-1; 6];
        let (nbytes, nfds) = stream.recv_with_fd(&mut len, &mut fds)?;
        if nbytes != 8 || nfds != 6 {
            throw!(ShamanError::HandshakeFailed)
        }
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

        let poll = Poll::new()?;
        let events = Events::with_capacity(128);
        let mut stream = UnixStream::from_std(stream);
        let tx = BufSender::new(tx);
        let rx = BufReceiver::new(rx);

        poll.registry()
            .register(&mut stream, SOCK, Interest::READABLE)?;
        poll.registry().register(
            &mut SourceFd(&tx.notifier().as_raw_fd()),
            TX,
            Interest::READABLE,
        )?;
        poll.registry().register(
            &mut SourceFd(&rx.notifier().as_raw_fd()),
            RX,
            Interest::READABLE,
        )?;

        Self {
            stream,
            tx,
            rx,
            poll,
            receivable: false,
            sendable: true,
            events,
        }
    }

    #[throws(ShamanError)]
    pub fn poll(&mut self, d: Option<Duration>) {
        self.poll.poll(&mut self.events, d)?;
        for event in self.events.iter() {
            match event.token() {
                SOCK if event.is_read_closed() => {
                    throw!(ConnectionClosed)
                }
                SOCK if event.is_readable() => {
                    // consume the data  if server send us anything
                    let mut buf = [0; 32];
                    while let Ok(32) = self.stream.read(&mut buf) {}
                }
                TX => self.sendable = true,
                RX => self.receivable = true,
                _ => unreachable!("Unknown Token"),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn recv(&mut self) -> Vec<u8> {
        self.recv_with(&mut |data| data.to_vec())?
    }

    // zero copy
    #[throws(ShamanError)]
    pub fn recv_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&[u8]) -> R,
    {
        loop {
            match self.rx.try_recv_with(f) {
                Ok(v) => break v,
                Err(WouldBlock) => {
                    self.receivable = false;
                    while !self.receivable {
                        self.poll(None)?;
                    }
                }
                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn recv_timeout(&mut self, d: Duration) -> Vec<u8> {
        let then = Instant::now();
        loop {
            match self.rx.try_recv() {
                Ok(v) => break v,
                Err(WouldBlock) => {
                    self.receivable = false;
                    while !self.receivable {
                        let poll_time = d - then.elapsed();
                        if poll_time < Duration::from_secs(0) {
                            throw!(WouldBlock)
                        } else {
                            self.poll(Some(poll_time))?;
                        }
                    }
                }
                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn try_recv(&mut self) -> Vec<u8> {
        self.try_recv_with(&mut |data| data.to_vec())?
    }

    // zero copy
    #[throws(ShamanError)]
    pub fn try_recv_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&[u8]) -> R,
    {
        match self.rx.try_recv_with(f) {
            Ok(v) => v,
            Err(WouldBlock) => {
                self.receivable = false;
                throw!(WouldBlock);
            }
            Err(e) => throw!(e),
        }
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, data: &[u8]) {
        loop {
            while !self.sendable {
                self.poll(None)?;
            }

            match self.tx.try_send(data) {
                Ok(_) => break,
                Err(WouldBlock) => self.sendable = false,
                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn try_send(&mut self, data: &[u8]) {
        match self.tx.try_send(data) {
            Ok(_) => {}
            Err(WouldBlock) => {
                self.sendable = false;
                throw!(WouldBlock);
            }
            Err(e) => throw!(e),
        }
    }

    #[throws(ShamanError)]
    pub fn send_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&mut [u8], &mut VecDeque<u8>, &mut usize) -> R,
    {
        loop {
            while !self.sendable {
                self.poll(None)?;
            }

            match self.tx.try_send_with(f) {
                Ok(r) => break r,
                Err(WouldBlock) => self.sendable = false,
                Err(e) => throw!(e),
            }
        }
    }

    /// f will be called if the channel has enough space.
    /// f should first write to the `&mut [u8]`. If full, write the rest to
    /// `&mut VecDeque<u8>`. Set total bytes wrote in `&mut usize`.
    #[throws(ShamanError)]
    pub fn try_send_with<F, R>(&mut self, f: &mut F) -> R
    where
        F: FnMut(&mut [u8], &mut VecDeque<u8>, &mut usize) -> R,
    {
        match self.tx.try_send_with(f) {
            Ok(r) => r,
            Err(WouldBlock) => {
                self.sendable = false;
                throw!(WouldBlock);
            }
            Err(e) => throw!(e),
        }
    }

    /// Get the RawFd to get the notification when the send side is not full.
    /// Useful for the epoll/mio based application
    pub fn send_notifier(&self) -> &File {
        self.tx.notifier()
    }

    /// Get the RawFd to get the notification when the recv side is not empty.
    /// Useful for the epoll/mio based application
    pub fn recv_notifier(&self) -> &File {
        self.rx.notifier()
    }
}
