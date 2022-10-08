mod client;
mod duplex;
mod errors;
mod server;
mod types;

pub use crate::types::*;
pub use client::ShamanClient;
pub use errors::ShamanError;
pub use server::{RequestHandler, ShamanServer, ShamanServerHandle};
use std::mem::size_of;

const SIZE: usize = size_of::<usize>();
