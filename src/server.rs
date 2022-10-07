use crate::{duplex::Duplex, errors::ShamanError, types::*, CAPACITY, SIZE};
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
use std::{
    collections::HashMap,
    fs::remove_file,
    io::{self},
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

pub trait RequestHandler {
    fn handle(&mut self, conn_id: usize, req: Request, handle: &ShamanServerHandle);
}

impl<T> RequestHandler for T
where
    T: FnMut(usize, Request, &ShamanServerHandle),
{
    fn handle(&mut self, conn_id: usize, req: Request, handle: &ShamanServerHandle) {
        self(conn_id, req, handle)
    }
}

pub struct Connection {
    pub(crate) stream: UnixStream,
    duplex: Duplex,
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
    resp_rx: StdReceiver<(Option<usize>, Response)>, // None if broadcast
    request_handler: H,

    handle: ShamanServerHandle,
}

#[derive(Clone)]
pub struct ShamanServerHandle {
    exit: Arc<AtomicBool>,
    pub tx: MSender<(Option<usize>, Response)>,
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
        t: (Option<usize>, Response),
    ) -> Result<(), SendError<(Option<usize>, Response)>> {
        self.tx.send(t)
    }
}

impl<H> ShamanServer<H>
where
    H: RequestHandler + Send + 'static,
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
    H: RequestHandler,
{
    #[throws(ShamanError)]
    pub fn new<P>(path: P, req_handler: H) -> (Self, ShamanServerHandle)
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
                request_handler: req_handler,
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
                    SERVER => {
                        let new_conns = self.incoming_handler()?;
                        for (id, tx_token, rx_token) in new_conns {
                            let conn = self.connections.get_mut(&id).unwrap();

                            self.poll.registry().register(
                                &mut conn.stream,
                                id,
                                Interest::READABLE,
                            )?;

                            self.poll.registry().register(
                                &mut SourceFd(&conn.duplex.tx.full_signal().as_raw_fd()),
                                tx_token,
                                Interest::READABLE,
                            )?;

                            self.poll.registry().register(
                                &mut SourceFd(&conn.duplex.rx.empty_signal().as_raw_fd()),
                                rx_token,
                                Interest::READABLE,
                            )?;

                            info!("Registered new connection {}", id.0);
                        }
                    }
                    WAKER => self.response_handler()?,
                    _ => self.ipc_handler(event)?,
                }
            }
        }
    }

    #[throws(ShamanError)]
    fn incoming_handler(&mut self) -> Vec<(Token, Token, Token)> {
        let mut new_conns = vec![];

        loop {
            match self.socket_listener.accept() {
                Ok((stream, _)) => {
                    let rx = match IPCReceiver::new(CAPACITY as usize) {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Cannot create receiver: {}", e);
                            continue;
                        }
                    };
                    let mr = rx.memfd().as_file().as_raw_fd();
                    let er = rx.empty_signal().as_raw_fd();
                    let fr = rx.full_signal().as_raw_fd();

                    let tx = match IPCSender::new(CAPACITY as usize) {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Cannot create sender: {}", e);
                            continue;
                        }
                    };
                    let ms = tx.memfd().as_file().as_raw_fd();
                    let es = tx.empty_signal().as_raw_fd();
                    let fs = tx.full_signal().as_raw_fd();

                    let rawfd = stream.as_raw_fd();
                    let stdstream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(rawfd) };
                    if let Err(e) =
                        stdstream.send_with_fd(&CAPACITY.to_ne_bytes(), &[mr, er, fr, ms, es, fs])
                    {
                        error!("Cannot send fds: {}", e);
                        continue;
                    };
                    forget(stdstream);

                    let id = self.next_token();
                    let tx_token = self.next_token();
                    let rx_token = self.next_token();

                    self.connections.insert(
                        id,
                        Connection {
                            stream,
                            duplex: Duplex::new(tx, rx),
                        },
                    );
                    self.tx_token_to_connection.insert(tx_token, id);
                    self.rx_token_to_connection.insert(rx_token, id);
                    new_conns.push((id, tx_token, rx_token));
                }
                Err(e) if would_block(&e) => break,
                Err(e) => throw!(e),
            }
        }

        new_conns
    }

    #[throws(ShamanError)]
    fn response_handler(&mut self) {
        while let Some(_) = self.queue.pop() {
            let (id, message) = self.resp_rx.recv().unwrap();

            let smsg = bincode::serialize(&message)?;
            match id {
                Some(id) => {
                    let connection = self.connections.get_mut(&Token(id)).unwrap();
                    connection.duplex.send(&smsg)?;
                }
                None => {
                    for connection in self.connections.values_mut() {
                        connection.duplex.send(&smsg)?;
                    }
                }
            }
        }
    }

    #[throws(ShamanError)]
    fn ipc_handler(&mut self, event: &Event) {
        if let Some(conn_id) = self.tx_token_to_connection.get(&event.token()) {
            let conn = self.connections.get_mut(conn_id).unwrap();
            conn.duplex.flush()?;
            return;
        }

        if let Some(conn_id) = self.rx_token_to_connection.get(&event.token()) {
            let conn = self.connections.get_mut(conn_id).unwrap();

            while let Some(data) = conn.duplex.recv()? {
                let req: Request = bincode::deserialize(&data[SIZE..])?;
                self.request_handler.handle(conn_id.0, req, &self.handle);
            }
            return;
        }

        if event.is_read_closed() {
            match self.connections.remove(&event.token()) {
                Some(mut conn) => {
                    info!("Connection {} closed", event.token().0);
                    self.poll.registry().deregister(&mut conn.stream)?;

                    self.poll
                        .registry()
                        .deregister(&mut SourceFd(&conn.duplex.tx.full_signal().as_raw_fd()))?;

                    self.poll
                        .registry()
                        .deregister(&mut SourceFd(&conn.duplex.rx.empty_signal().as_raw_fd()))?;
                }
                None => {
                    error!("Connection {} not found", event.token().0)
                }
            };

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
