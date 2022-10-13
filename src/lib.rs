#[cfg(feature = "tokio")]
mod async_client;
mod client;
mod duplex;
mod errors;
mod server;
mod types;

pub use crate::types::*;
#[cfg(feature = "tokio")]
pub use async_client::ShamanAsyncClient;
pub use client::ShamanClient;
pub use duplex::write_slice;
pub use errors::ShamanError;
pub use server::{MessageHandler, ShamanServer, ShamanServerHandle};
use std::{io, mem::size_of};

const SIZE: usize = size_of::<usize>();

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
