use thiserror::Error;

#[derive(Error, Debug)]
pub enum ShamanError {
    #[error("Connection closed")]
    ConnectionClosed,

    #[error("Would block")]
    WouldBlock,

    #[error("Connection {0} not found")]
    ConnectionNotFound(usize),

    #[error("Connection {0} not established")]
    ConnectionNotEstablished(usize),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    IPCError(#[from] shmem_ipc::Error),
}
