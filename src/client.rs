use crate::duplex::Duplex;
use crate::{
    errors::ShamanError::{self, *},
    SIZE,
};
use fehler::{throw, throws};
use mio::net::UnixStream;
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use sendfd::RecvWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::io::Write;
use std::mem::forget;
use std::os::unix::prelude::AsRawFd;
use std::time::Duration;
use std::{
    fs::File,
    os::unix::{io::FromRawFd, prelude::RawFd},
    path::Path,
};

const SOCK: Token = Token(0);
const TX: Token = Token(1);
const RX: Token = Token(2);

pub struct ShamanClient {
    _stream: UnixStream,
    duplex: Duplex,
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
        let mut stream = UnixStream::connect(path)?;

        let mut poll = Poll::new()?;
        let mut events = Events::with_capacity(128);
        poll.registry()
            .register(&mut stream, SOCK, Interest::WRITABLE)?;
        poll.poll(&mut events, Some(Duration::from_millis(100)))?;
        if events.is_empty() {
            throw!(ConnectionTimeout)
        }
        stream.write(&capacity.to_ne_bytes())?;

        poll.registry().deregister(&mut stream)?;
        poll.registry()
            .register(&mut stream, SOCK, Interest::READABLE)?;
        poll.poll(&mut events, Some(Duration::from_millis(100)))?;
        if events.is_empty() {
            throw!(ConnectionTimeout)
        }

        let mut len = [0; SIZE];
        let mut fds: [RawFd; 6] = [-1; 6];

        let rawfd = stream.as_raw_fd();
        let stdstream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(rawfd) };
        stdstream.recv_with_fd(&mut len, &mut fds)?;
        forget(stdstream);

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

        poll.registry().register(
            &mut SourceFd(&tx.full_signal().as_raw_fd()),
            TX,
            Interest::READABLE,
        )?;
        poll.registry().register(
            &mut SourceFd(&rx.empty_signal().as_raw_fd()),
            RX,
            Interest::READABLE,
        )?;

        Self {
            _stream: stream,
            duplex: Duplex::new(tx, rx),
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
                SOCK => {
                    if event.is_read_closed() {
                        throw!(ConnectionClosed)
                    }
                }
                TX => self.sendable = true,
                RX => self.receivable = true,
                _ => unreachable!("Unknown Token"),
            }
        }
    }

    #[throws(ShamanError)]
    pub fn recv(&mut self) -> Vec<u8> {
        loop {
            while !self.receivable {
                self.poll(None)?;
            }

            match self.duplex.try_recv()? {
                Some(v) => break v,
                None => self.receivable = false,
            }
        }
    }

    #[throws(ShamanError)]
    pub fn recv_timeout(&mut self, d: Duration) -> Option<Vec<u8>> {
        if self.receivable {
            match self.duplex.try_recv()? {
                Some(v) => return Some(v),
                None => self.receivable = false,
            }
        }
        self.poll(Some(d))?;

        self.duplex.try_recv()?
    }

    #[throws(ShamanError)]
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        if self.receivable {
            match self.duplex.try_recv()? {
                Some(v) => return Some(v),
                None => self.receivable = false,
            }
        }
        self.poll(Some(Duration::from_secs(0)))?;

        self.duplex.try_recv()?
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, data: &[u8]) {
        if self.sendable {
            match self.duplex.send(data) {
                Ok(_) => return,
                Err(ShamanError::SenderFull) => {
                    self.sendable = false;
                }
                Err(e) => throw!(e),
            }
        }
        while !self.sendable {
            self.poll(None)?;
        }

        self.duplex.send(data)?;
    }

    #[throws(ShamanError)]
    pub fn try_send(&mut self, data: &[u8]) -> bool {
        if self.sendable {
            match self.duplex.send(data) {
                Ok(_) => return true,
                Err(ShamanError::SenderFull) => {
                    self.sendable = false;
                    return false;
                }
                Err(e) => throw!(e),
            }
        }
        self.poll(Some(Duration::from_secs(0)))?;
        match self.duplex.send(data) {
            Ok(_) => true,
            Err(ShamanError::SenderFull) => {
                self.sendable = false;
                false
            }
            Err(e) => throw!(e),
        }
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
