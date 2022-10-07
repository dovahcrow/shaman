mod client;
mod duplex;
mod errors;
mod server;
mod types;

pub use crate::types::*;
pub use client::ShamanClient;
pub use errors::ShamanError;
pub use server::{ShamanServer, ShamanServerHandle};
use std::mem::size_of;

const CAPACITY: usize = 1 << 19; // 512k buffer
const SIZE: usize = size_of::<usize>();
