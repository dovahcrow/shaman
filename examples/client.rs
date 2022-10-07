use std::{thread::sleep, time::Duration};

use anyhow::Error;
use fehler::throws;
use shaman::ShamanClient;

#[throws(Error)]
fn main() {
    env_logger::init();
    let _c = ShamanClient::new("/tmp/shaman.sock")?;
    sleep(Duration::from_secs(10));
}
