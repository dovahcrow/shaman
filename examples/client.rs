mod shared;

use anyhow::Error;
use fehler::throws;
use shaman::ShamanClient;
use shared::{Request, Response};
use std::str;

#[allow(unreachable_code)]
#[throws(Error)]
fn main() {
    env_logger::init();
    let mut client = ShamanClient::new("/tmp/shaman.sock", 1 << 4)?;

    client.send(&bincode::serialize(&Request {
        id: 0,
        method: "hello".to_owned(),
        params: b"world".to_vec(),
    })?)?;

    println!("Request sent");

    let data = client.recv()?;
    let resp: Response = bincode::deserialize(&data)?;
    if let Response::Success { id, data } = resp {
        assert_eq!(id, 0);
        println!("Response: {}", str::from_utf8(&data)?);
    };

    loop {
        let msg = client.recv()?;
        let resp: Response = bincode::deserialize(&msg)?;
        if let Response::Subscription { channel, data } = resp {
            println!("{}: {}", channel, str::from_utf8(&data)?);
        }
    }
}
