use anyhow::Error;
use fehler::throws;
use shaman::ShamanServer;

#[throws(Error)]
fn main() {
    env_logger::init();
    let (s, _x, _y) = ShamanServer::new("/tmp/shaman.sock")?;
    s.start()?;
}
