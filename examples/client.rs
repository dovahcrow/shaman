mod shared;

use anyhow::Error;
use fehler::throws;
use shaman::ShamanClient;
use shared::{Request, Response};
use std::{
    str,
    time::{Duration, Instant},
};

#[throws(Error)]
fn main() {
    env_logger::init();
    let mut client = ShamanClient::new("/tmp/shaman.sock")?;
    let then = Instant::now();
    client.send(&bincode::serialize(&Request {
        id: 0,
        method: "hello".to_owned(),
        params: b"world".to_vec(),
    })?)?;

    let data = client.recv()?.unwrap();
    let resp: Response = bincode::deserialize(&data)?;
    if let Response::Success { id, data } = resp {
        assert_eq!(id, 0);
        println!("Response: {}", str::from_utf8(&data)?);
    };

    while then.elapsed() < Duration::from_secs(10) {
        let maybe_msg = client.recv()?;
        if let Some(msg) = maybe_msg {
            let resp: Response = bincode::deserialize(&msg)?;
            if let Response::Subscription { channel, data } = resp {
                println!("{}: {}", channel, str::from_utf8(&data)?);
            }
        }
    }
}
