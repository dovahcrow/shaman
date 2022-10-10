use crate::{
    duplex::Duplex,
    errors::ShamanError::{self, *},
    types::*,
    would_block, SIZE,
};
use fehler::{throw, throws};
use log::{error, info};
use mio::{
    event::Event, net::UnixListener, net::UnixStream, unix::SourceFd, Events, Interest, Poll, Token,
};
use parking_lot::Mutex;
use sendfd::SendWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::collections::VecDeque;
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
    fn on_connect(&mut self, _: &mut ShamanServerSendHandle) {}
    fn on_data(&mut self, _: &[u8], _: &mut ShamanServerSendHandle<'_>) {}
    fn on_disconnect(&mut self, _: ConnId) {}
}

impl<T> MessageHandler for T
where
    T: FnMut(&[u8], &mut ShamanServerSendHandle<'_>),
{
    fn on_data(&mut self, data: &[u8], handle: &mut ShamanServerSendHandle<'_>) {
        self(data, handle)
    }
}

pub struct Connection {
    pub(crate) stream: UnixStream,
    duplex: Option<Duplex>,
}

pub struct ShamanServer<H> {
    socket_path: PathBuf,

    socket_listener: UnixListener,
    connections: Arc<Mutex<HashMap<Token, Connection>>>, // connection id -> connection
    tx_token_to_connection: HashMap<Token, Token>,       // tx poll token -> connection id
    rx_token_to_connection: HashMap<Token, Token>,       // rx poll token -> connection id

    next_poll_token: usize,

    // communication with outside
    poll: Poll,
    message_handler: H,

    handle: ShamanServerHandle,
}

pub struct ShamanServerSendHandle<'a> {
    conn_id: ConnId,
    tx: &'a mut IPCSender<u8>,
    tx_buf: &'a mut VecDeque<u8>,
}

impl<'a> ShamanServerSendHandle<'a> {
    pub fn conn_id(&self) -> ConnId {
        self.conn_id
    }

    #[throws(ShamanError)]
    pub fn try_send(&mut self, data: &[u8]) {
        Duplex::try_send(&mut self.tx, &mut self.tx_buf, data)?;
    }
}

#[derive(Clone)]
pub struct ShamanServerHandle {
    exit: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<Token, Connection>>>,
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
        let mut connections = self.connections.lock();
        let conn = connections
            .get_mut(&Token(conn_id))
            .ok_or(ConnectionNotFound(conn_id))?;

        let duplex = match conn.duplex.as_mut() {
            Some(duplex) => duplex,
            None => throw!(ConnectionNotEstablished(conn_id)),
        };

        Duplex::try_send(&mut duplex.tx, &mut duplex.tx_buf, data)?;
    }

    #[throws(ShamanError)]
    pub fn try_broadcast(&self, data: &[u8]) {
        let mut connections = self.connections.lock();
        let mut last_error = None;
        for conn in connections.values_mut() {
            let duplex = match conn.duplex.as_mut() {
                Some(duplex) => duplex,
                None => {
                    // The connection is still being established, skip
                    continue;
                }
            };

            match Duplex::try_send(&mut duplex.tx, &mut duplex.tx_buf, data) {
                Ok(_) => {}
                Err(e) => last_error = Some(e),
            }; // todo: how to report all errors
        }
        if let Some(e) = last_error {
            throw!(e)
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
    #[throws(ShamanError)]
    pub fn new<P>(path: P, message_handler: H) -> (Self, ShamanServerHandle)
    where
        P: AsRef<Path>,
    {
        let path = path.as_ref();

        let connections: Arc<Mutex<HashMap<Token, Connection>>> = Arc::default();
        let mut listener = UnixListener::bind(path)?;

        let poll = Poll::new()?;

        poll.registry()
            .register(&mut listener, SERVER, Interest::READABLE)?;

        let exit = Arc::new(AtomicBool::new(false));
        let handle = ShamanServerHandle {
            connections: connections.clone(),
            exit,
        };

        (
            Self {
                socket_path: path.to_owned(),
                socket_listener: listener,
                connections,
                next_poll_token: 1,
                rx_token_to_connection: HashMap::new(),
                tx_token_to_connection: HashMap::new(),
                poll,
                message_handler,
                handle: handle.clone(),
            },
            handle,
        )
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
                    let mut connections = self.connections.lock();

                    connections.insert(
                        id,
                        Connection {
                            stream,
                            duplex: None,
                        },
                    );
                    info!("Connection {} connected", id.0);
                }
                Err(e) if would_block(&e) => break,
                Err(e) => throw!(e),
            }
        }
    }

    #[throws(ShamanError)]
    fn ipc_handler(&mut self, event: &Event) {
        if let Some(conn_id) = self.tx_token_to_connection.get(&event.token()) {
            let mut connections = self.connections.lock();
            let conn = connections.get_mut(conn_id).unwrap();
            let duplex = conn.duplex.as_mut().unwrap();

            // clear the eventfd
            let mut buf = [0; 8];
            duplex.send_notifier().read_exact(&mut buf)?;

            Duplex::flush(&mut duplex.tx, &mut duplex.tx_buf)?;
            return;
        }

        if let Some(conn_id) = self.rx_token_to_connection.get(&event.token()) {
            let mut connections = self.connections.lock();
            let conn = connections.get_mut(conn_id).unwrap();
            let duplex = conn.duplex.as_mut().unwrap();

            // clear the eventfd
            let mut buf = [0; 8];
            duplex.recv_notifier().read_exact(&mut buf)?;

            let mut handle = ShamanServerSendHandle {
                conn_id: conn_id.0,
                tx: &mut duplex.tx,
                tx_buf: &mut duplex.tx_buf,
            };
            loop {
                let ret = Duplex::try_recv_with(&mut duplex.rx, &mut duplex.rx_buf, |data| {
                    self.message_handler.on_data(data, &mut handle)
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
            let mut connections = self.connections.lock();
            let mut conn = match connections.remove(&event.token()) {
                Some(conn) => conn,
                None => {
                    error!("Connection {} not found", event.token().0);
                    return;
                }
            };

            info!("Connection {} closed", event.token().0);
            self.poll.registry().deregister(&mut conn.stream)?;

            if let Some(duplex) = conn.duplex {
                self.poll
                    .registry()
                    .deregister(&mut SourceFd(&duplex.send_notifier().as_raw_fd()))?;

                self.poll
                    .registry()
                    .deregister(&mut SourceFd(&duplex.recv_notifier().as_raw_fd()))?;
            }

            self.message_handler.on_disconnect(event.token().0);
            return;
        }

        if event.is_readable() {
            let conn_id = event.token().0;

            let mut connections = self.connections.lock();
            let conn = match connections.get_mut(&event.token()) {
                Some(conn) => conn,
                None => {
                    error!("Connection {} not found", conn_id);
                    return;
                }
            };

            let mut capacity = [0; SIZE];
            conn.stream.read_exact(&mut capacity)?;
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

            let rawfd = conn.stream.as_raw_fd();
            let stdstream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(rawfd) };
            if let Err(e) = stdstream.send_with_fd(
                &capacity.to_ne_bytes(),
                &[
                    rx.memfd().as_file().as_raw_fd(),
                    rx.empty_signal().as_raw_fd(),
                    rx.full_signal().as_raw_fd(),
                    tx.memfd().as_file().as_raw_fd(),
                    tx.empty_signal().as_raw_fd(),
                    tx.full_signal().as_raw_fd(),
                ],
            ) {
                error!("Cannot send fds: {}", e);
                return;
            };
            forget(stdstream);

            conn.duplex = Some(Duplex::new(tx, rx));

            // let tx_token = self.next_token();
            // let rx_token = self.next_token();
            let tx_token = Token(self.next_poll_token);
            self.next_poll_token += 1;
            let rx_token = Token(self.next_poll_token);
            self.next_poll_token += 1;

            self.tx_token_to_connection.insert(tx_token, Token(conn_id));
            self.rx_token_to_connection.insert(rx_token, Token(conn_id));

            self.poll.registry().register(
                &mut SourceFd(&conn.duplex.as_ref().unwrap().send_notifier().as_raw_fd()),
                tx_token,
                Interest::READABLE,
            )?;

            self.poll.registry().register(
                &mut SourceFd(&conn.duplex.as_ref().unwrap().recv_notifier().as_raw_fd()),
                rx_token,
                Interest::READABLE,
            )?;
            let duplex = conn.duplex.as_mut().unwrap();
            let mut handle = ShamanServerSendHandle {
                conn_id: conn_id,
                tx: &mut duplex.tx,
                tx_buf: &mut duplex.tx_buf,
            };

            self.message_handler.on_connect(&mut handle);
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
