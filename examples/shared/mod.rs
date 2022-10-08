use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    pub params: Vec<u8>,
}

#[derive(Deserialize, Serialize)]
pub enum Response {
    Success { id: u64, data: Vec<u8> },
    Error { id: u64, code: u64 },
    Subscription { channel: String, data: Vec<u8> },
}
