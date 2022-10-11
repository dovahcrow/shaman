use anyhow::Error;
use fehler::throws;
use shaman::ShamanClient;
use std::str;

#[allow(unreachable_code)]
#[throws(Error)]
fn main() {
    env_logger::init();
    let mut client = ShamanClient::new("/tmp/shaman.sock", 1 << 4)?;

    client.send(b"hello world")?;

    println!("Request sent");

    loop {
        let msg = client.recv()?;

        println!("{}", str::from_utf8(&msg)?);
    }
}
