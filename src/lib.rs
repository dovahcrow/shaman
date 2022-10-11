mod client;
mod duplex;
mod errors;
mod server;
mod types;

pub use crate::types::*;
pub use client::ShamanClient;
pub use errors::ShamanError;
pub use server::{MessageHandler, ShamanServer, ShamanServerHandle};
use std::{io, mem::size_of};

const SIZE: usize = size_of::<usize>();

fn would_block(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
}
