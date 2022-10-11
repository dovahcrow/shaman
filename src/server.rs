use crate::{
    duplex::{BufReceiver, BufSender},
    errors::ShamanError::{self, *},
    types::*,
    would_block, SIZE,
};
use dashmap::DashMap;
use fehler::{throw, throws};
use log::{error, info};
use mio::{
    event::Event, net::UnixListener, net::UnixStream, unix::SourceFd, Events, Interest, Poll, Token,
};
use sendfd::SendWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::io::Read;
use std::{
    collections::HashMap,
    fs::remove_file,
    mem::forget,
    os::unix::io::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::spawn,
    time::Duration,
};

const SERVER: Token = Token(0);

pub trait MessageHandler {
    fn on_connect(&mut self, _: ConnId) {}
    fn on_data(&mut self, _: ConnId, _: &[u8]) {}
    fn on_disconnect(&mut self, _: ConnId) {}
}

impl<T> MessageHandler for T
where
    T: FnMut(ConnId, &[u8]),
{
    fn on_data(&mut self, conn_id: ConnId, data: &[u8]) {
        self(conn_id, data)
    }
}

#[derive(Clone)]
pub struct ShamanServerHandle {
    exit: Arc<AtomicBool>,
    senders: Arc<DashMap<Token, (Token, BufSender)>>,
}

impl ShamanServerHandle {
    pub fn is_exited(&self) -> bool {
        self.exit.load(Ordering::Relaxed)
    }

    pub fn exit(&self) {
        self.exit.store(true, Ordering::Relaxed);
    }

    #[throws(ShamanError)]
    pub fn try_send(&self, conn_id: ConnId, data: &[u8]) {
        let mut tx = self
            .senders
            .get_mut(&Token(conn_id))
            .ok_or(ConnectionNotFound(conn_id))?;

        tx.1.try_send(data)?;
    }

    #[throws(ShamanError)]
    pub fn try_broadcast(&self, data: &[u8]) {
        let mut last_error = None;
        for mut pair in self.senders.iter_mut() {
            let tx = &mut pair.value_mut().1;

            match tx.try_send(data) {
                Ok(_) => {}
                Err(e) => last_error = Some(e),
            }; // todo: how to report all errors
        }
        if let Some(e) = last_error {
            throw!(e)
        }
    }
}

pub struct ShamanServer<H> {
    socket_path: PathBuf,

    socket_listener: UnixListener,
    streams: HashMap<Token, UnixStream>, // connection id -> stream
    receivers: HashMap<Token, (Token, BufReceiver)>, // connection id -> receiver
    senders: Arc<DashMap<Token, (Token, BufSender)>>, // connection id -> sender
    tx_token_to_connection: HashMap<Token, Token>, // tx poll token -> connection id
    rx_token_to_connection: HashMap<Token, Token>, // rx poll token -> connection id

    next_poll_token: usize,

    // communication with outside
    poll: Poll,
    message_handler: Option<H>,

    handle: ShamanServerHandle,
}

impl<H> ShamanServer<H> {
    pub fn handle(&self) -> ShamanServerHandle {
        self.handle.clone()
    }

    #[throws(ShamanError)]
    pub fn new<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref();

        let senders: Arc<DashMap<Token, (Token, BufSender)>> = Arc::default();
        let mut listener = UnixListener::bind(path)?;

        let poll = Poll::new()?;

        poll.registry()
            .register(&mut listener, SERVER, Interest::READABLE)?;

        let exit = Arc::new(AtomicBool::new(false));
        let handle = ShamanServerHandle {
            senders: senders.clone(),
            exit,
        };

        Self {
            socket_path: path.to_owned(),
            socket_listener: listener,

            streams: HashMap::new(),
            senders,
            receivers: HashMap::new(),

            next_poll_token: 1,
            rx_token_to_connection: HashMap::new(),
            tx_token_to_connection: HashMap::new(),
            poll,
            message_handler: None,
            handle,
        }
    }
}

impl<H> ShamanServer<H>
where
    H: MessageHandler + Send + 'static,
{
    pub fn spawn(self) {
        spawn(move || match self.start() {
            Err(e) => error!("ShamanServer exited due to: {:?}", e),
            Ok(_) => {}
        });
    }
}

impl<H> ShamanServer<H>
where
    H: MessageHandler,
{
    pub fn set_message_handler(&mut self, message_handler: H) {
        self.message_handler = Some(message_handler)
    }

    #[allow(unreachable_code)]
    #[throws(ShamanError)]
    pub fn start(mut self) {
        let mut events = Events::with_capacity(128);

        while !self.handle.is_exited() {
            self.poll
                .poll(&mut events, Some(Duration::from_millis(100)))?;

            for event in events.iter() {
                match event.token() {
                    SERVER => self.incoming_handler()?,
                    _ => self.ipc_handler(event)?,
                }
            }
        }
    }

    #[throws(ShamanError)]
    fn incoming_handler(&mut self) {
        loop {
            match self.socket_listener.accept() {
                Ok((mut stream, _)) => {
                    let id = self.next_token();
                    self.poll
                        .registry()
                        .register(&mut stream, id, Interest::READABLE)?;

                    self.streams.insert(id, stream);
                    info!("Connection {} connected", id.0);
                }
                Err(e) if would_block(&e) => break,
                Err(e) => throw!(e),
            }
        }
    }

    // This function first locks the connection then call the handler
    // dead lock can happen if the handler locks its internal state
    // another then another process locks the handler's internal state first
    // then call the send method on the ShamanServerHandle
    #[throws(ShamanError)]
    fn ipc_handler(&mut self, event: &Event) {
        if let Some(conn_id) = self.tx_token_to_connection.get(&event.token()) {
            let mut tx = self.senders.get_mut(conn_id).unwrap();
            tx.1.flush()?;
            return;
        }

        if let Some(conn_id) = self.rx_token_to_connection.get(&event.token()) {
            let (_, ref mut rx) = self.receivers.get_mut(conn_id).unwrap();

            loop {
                let ret = rx.try_recv_with(|data| {
                    if let Some(mh) = self.message_handler.as_mut() {
                        mh.on_data(conn_id.0, data)
                    }
                });

                match ret {
                    Ok(_) => {}
                    Err(WouldBlock) => break,
                    Err(e) => throw!(e),
                }
            }

            return;
        }

        if event.is_read_closed() {
            let conn_id = event.token().0;

            let mut stream = match self.streams.remove(&Token(conn_id)) {
                Some(stream) => stream,
                None => {
                    error!("Connection {} not found", conn_id);
                    return;
                }
            };

            info!("Connection {} closed", conn_id);
            self.poll.registry().deregister(&mut stream)?;

            if let Some((_, (token, sender))) = self.senders.remove(&Token(conn_id)) {
                self.poll
                    .registry()
                    .deregister(&mut SourceFd(&sender.notifier().as_raw_fd()))?;
                self.tx_token_to_connection.remove(&token);
            }
            if let Some((token, receiver)) = self.receivers.remove(&Token(conn_id)) {
                self.poll
                    .registry()
                    .deregister(&mut SourceFd(&receiver.notifier().as_raw_fd()))?;
                self.rx_token_to_connection.remove(&token);
            }

            if let Some(mh) = self.message_handler.as_mut() {
                mh.on_disconnect(conn_id);
            }
            return;
        }

        if event.is_readable() {
            let conn_id = event.token().0;

            let stream = match self.streams.get_mut(&event.token()) {
                Some(conn) => conn,
                None => {
                    error!("Connection {} not found", conn_id);
                    return;
                }
            };

            let mut capacity = [0; SIZE];
            stream.read_exact(&mut capacity)?;

            let capacity = usize::from_ne_bytes(capacity);

            let rx = match IPCReceiver::new(capacity) {
                Ok(r) => r,
                Err(e) => {
                    error!("Cannot create receiver: {}", e);
                    return;
                }
            };

            let tx = match IPCSender::new(capacity) {
                Ok(r) => r,
                Err(e) => {
                    error!("Cannot create sender: {}", e);
                    return;
                }
            };

            let fds = &[
                rx.memfd().as_file().as_raw_fd(),
                rx.empty_signal().as_raw_fd(),
                rx.full_signal().as_raw_fd(),
                tx.memfd().as_file().as_raw_fd(),
                tx.empty_signal().as_raw_fd(),
                tx.full_signal().as_raw_fd(),
            ];

            for fd in fds {
                unsafe {
                    let flags = libc::fcntl(*fd, libc::F_GETFL, 0);
                    libc::fcntl(*fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }

            let rawfd = stream.as_raw_fd();
            let stdstream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(rawfd) };
            if let Err(e) = stdstream.send_with_fd(&capacity.to_ne_bytes(), fds) {
                error!("Cannot send fds: {}", e);
                return;
            };
            forget(stdstream);

            let bufsender = BufSender::new(tx);
            let bufreceiver = BufReceiver::new(rx);

            let tx_token = Token(self.next_poll_token);
            self.next_poll_token += 1;
            let rx_token = Token(self.next_poll_token);
            self.next_poll_token += 1;

            self.poll.registry().register(
                &mut SourceFd(&bufsender.notifier().as_raw_fd()),
                tx_token,
                Interest::READABLE,
            )?;

            self.poll.registry().register(
                &mut SourceFd(&bufreceiver.notifier().as_raw_fd()),
                rx_token,
                Interest::READABLE,
            )?;

            self.senders.insert(Token(conn_id), (tx_token, bufsender));
            self.receivers
                .insert(Token(conn_id), (tx_token, bufreceiver));

            self.tx_token_to_connection.insert(tx_token, Token(conn_id));
            self.rx_token_to_connection.insert(rx_token, Token(conn_id));

            if let Some(mh) = self.message_handler.as_mut() {
                mh.on_connect(conn_id);
            }
            info!(
                "Connection {} established with capacity: {}",
                conn_id, capacity
            );
            return;
        }

        error!("Unhandled event: {:?}", event);
    }

    fn next_token(&mut self) -> Token {
        let token = Token(self.next_poll_token);
        self.next_poll_token += 1;
        token
    }
}

impl<H> Drop for ShamanServer<H> {
    fn drop(&mut self) {
        let _ = remove_file(&self.socket_path);
    }
}
