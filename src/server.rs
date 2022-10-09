use crate::{duplex::Duplex, errors::ShamanError, types::*, SIZE};
use fehler::{throw, throws};
use log::{error, info};
use mio::{
    event::Event, net::UnixListener, net::UnixStream, unix::SourceFd, Events, Interest, Poll,
    Token, Waker,
};
use mio_misc::{
    channel::{channel as mchannel, SendError, Sender as MSender},
    queue::NotificationQueue,
    NotificationId,
};
use sendfd::SendWithFd;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::io::Read;
use std::{
    collections::HashMap,
    fs::remove_file,
    io,
    mem::forget,
    os::unix::io::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver as StdReceiver,
        Arc,
    },
    thread::spawn,
    time::Duration,
};

const SERVER: Token = Token(0);
const WAKER: Token = Token(1);

pub trait MessageHandler {
    fn on_connect(&mut self, _: ConnId, _: &ShamanServerHandle) {}
    fn on_data(&mut self, _: ConnId, _: &[u8], _: &ShamanServerHandle) {}
    fn on_disconnect(&mut self, _: ConnId) {}
}

impl<T> MessageHandler for T
where
    T: FnMut(ConnId, &[u8], &ShamanServerHandle),
{
    fn on_data(&mut self, conn_id: ConnId, data: &[u8], handle: &ShamanServerHandle) {
        self(conn_id, data, handle)
    }
}

pub struct Connection {
    pub(crate) stream: UnixStream,
    duplex: Option<Duplex>,
}

pub struct ShamanServer<H> {
    socket_path: PathBuf,

    socket_listener: UnixListener,
    connections: HashMap<Token, Connection>, // connection id -> connection
    tx_token_to_connection: HashMap<Token, Token>, // tx poll token -> connection id
    rx_token_to_connection: HashMap<Token, Token>, // rx poll token -> connection id

    next_poll_token: usize,

    // communication with outside
    poll: Poll,
    queue: Arc<NotificationQueue>, // Notification queue for resp_rx
    resp_rx: StdReceiver<(Option<ConnId>, Vec<u8>)>, // None if broadcast
    message_handler: H,

    handle: ShamanServerHandle,
}

#[derive(Clone)]
pub struct ShamanServerHandle {
    exit: Arc<AtomicBool>,
    pub tx: MSender<(Option<ConnId>, Vec<u8>)>,
}

impl ShamanServerHandle {
    pub fn is_exited(&self) -> bool {
        self.exit.load(Ordering::Relaxed)
    }

    pub fn exit(&self) {
        self.exit.store(true, Ordering::Relaxed);
    }

    pub fn send(
        &self,
        t: (Option<ConnId>, Vec<u8>),
    ) -> Result<(), SendError<(Option<ConnId>, Vec<u8>)>> {
        self.tx.send(t)
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

        let connections = HashMap::new();
        let mut listener = UnixListener::bind(path)?;

        let poll = Poll::new()?;

        poll.registry()
            .register(&mut listener, SERVER, Interest::READABLE)?;

        let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
        let queue = Arc::new(NotificationQueue::new(waker));
        let (resp_tx, resp_rx) = mchannel(queue.clone(), NotificationId::gen_next());

        let exit = Arc::new(AtomicBool::new(false));
        let handle = ShamanServerHandle { tx: resp_tx, exit };

        (
            Self {
                socket_path: path.to_owned(),
                socket_listener: listener,
                connections,
                next_poll_token: 2,
                rx_token_to_connection: HashMap::new(),
                tx_token_to_connection: HashMap::new(),
                poll,
                queue,
                resp_rx,
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
                    WAKER => self.response_handler()?,
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
                    self.connections.insert(
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
    fn response_handler(&mut self) {
        while let Some(_) = self.queue.pop() {}
        while let Ok((id, message)) = self.resp_rx.try_recv() {
            match id {
                Some(id) => {
                    let connection = match self.connections.get_mut(&Token(id)) {
                        Some(conn) => conn,
                        None => continue,
                    };
                    let duplex = match connection.duplex.as_mut() {
                        Some(duplex) => duplex,
                        None => {
                            error!("Connection {} not established", id);
                            continue;
                        }
                    };

                    match duplex.send(&message) {
                        Ok(_) => {}
                        Err(ShamanError::SenderFull) => {
                            error!("Sender for connection {} is full, drop message", id);
                        }
                        Err(e) => throw!(e),
                    };
                }
                None => {
                    for (id, connection) in &mut self.connections {
                        let duplex = match connection.duplex.as_mut() {
                            Some(duplex) => duplex,
                            None => {
                                continue;
                            }
                        };

                        match duplex.send(&message) {
                            Ok(_) => {}
                            Err(ShamanError::SenderFull) => {
                                error!("Sender for connection {} is full, drop message", id.0);
                            }
                            Err(e) => throw!(e),
                        };
                    }
                }
            }
        }
    }

    #[throws(ShamanError)]
    fn ipc_handler(&mut self, event: &Event) {
        if let Some(conn_id) = self.tx_token_to_connection.get(&event.token()) {
            let conn = self.connections.get_mut(conn_id).unwrap();
            conn.duplex.as_mut().unwrap().flush()?;
            return;
        }

        if let Some(conn_id) = self.rx_token_to_connection.get(&event.token()) {
            let conn = self.connections.get_mut(conn_id).unwrap();

            while let Some(_) =
                conn.duplex.as_mut().unwrap().try_recv_with(|data| {
                    self.message_handler.on_data(conn_id.0, data, &self.handle)
                })?
            {}
            return;
        }

        if event.is_read_closed() {
            let mut conn = match self.connections.remove(&event.token()) {
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
                    .deregister(&mut SourceFd(&duplex.tx.full_signal().as_raw_fd()))?;

                self.poll
                    .registry()
                    .deregister(&mut SourceFd(&duplex.rx.empty_signal().as_raw_fd()))?;
            }

            self.message_handler.on_disconnect(event.token().0);
            return;
        }

        if event.is_readable() {
            let id = event.token().0;

            let conn = match self.connections.get_mut(&event.token()) {
                Some(conn) => conn,
                None => {
                    error!("Connection {} not found", id);
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

            self.tx_token_to_connection.insert(tx_token, Token(id));
            self.rx_token_to_connection.insert(rx_token, Token(id));

            self.poll.registry().register(
                &mut SourceFd(&conn.duplex.as_ref().unwrap().tx.full_signal().as_raw_fd()),
                tx_token,
                Interest::READABLE,
            )?;

            self.poll.registry().register(
                &mut SourceFd(&conn.duplex.as_ref().unwrap().rx.empty_signal().as_raw_fd()),
                rx_token,
                Interest::READABLE,
            )?;

            self.message_handler
                .on_connect(event.token().0, &self.handle);
            info!("Connection {} established with capacity: {}", id, capacity);
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

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
