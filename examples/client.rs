use anyhow::Error;
use fehler::throws;
use shaman::{Response, ShamanClient};
use std::{
    str,
    time::{Duration, Instant},
};

#[throws(Error)]
fn main() {
    env_logger::init();
    let mut client = ShamanClient::new("/tmp/shaman.sock")?;
    let then = Instant::now();
    client.send(0, "hello", b"world")?;
    if let Response::Success { id, data } = client.recv()?.unwrap() {
        assert_eq!(id, 0);
        println!("Response: {}", str::from_utf8(&data)?);
    };

    while then.elapsed() < Duration::from_secs(10) {
        let maybe_msg = client.recv()?;
        if let Some(Response::Subscription { channel, data }) = maybe_msg {
            println!("{}: {}", channel, str::from_utf8(&data)?);
        }
    }
}
