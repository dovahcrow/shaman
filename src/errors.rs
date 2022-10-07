use thiserror::Error;

#[derive(Error, Debug)]
pub enum ShamanError {
    #[error("Bytes too short, expected {0}, got {1}")]
    BytesTooShort(usize, usize),

    #[error("The sender is full")]
    SenderFull,

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    IPCError(#[from] shmem_ipc::Error),

    #[error(transparent)]
    BincodeError(#[from] Box<bincode::ErrorKind>),
}
