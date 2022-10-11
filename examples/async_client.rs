use anyhow::Error;
use fehler::throws;
use shaman::ShamanAsyncClient;
use std::str;

#[allow(unreachable_code)]
#[throws(Error)]
#[tokio::main]
async fn main() {
    env_logger::init();
    let mut client = ShamanAsyncClient::new("/tmp/shaman.sock", 1 << 4).await?;

    client.send(b"hello world").await?;

    println!("Request sent");

    loop {
        let msg = client.recv().await?;

        println!("{}", str::from_utf8(&msg)?);
    }
}
