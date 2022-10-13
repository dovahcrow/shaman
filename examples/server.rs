use anyhow::Error;
use fehler::throws;
use shaman::{ConnId, MessageHandler, ShamanServer, ShamanServerHandle};
use std::{
    fs::remove_file,
    str,
    thread::sleep,
    time::{Duration, Instant},
};

struct Handler {
    h: ShamanServerHandle,
}

impl MessageHandler for Handler {
    fn on_data(&mut self, conn_id: ConnId, data: &[u8]) {
        println!("Recv from client: {}", str::from_utf8(data).unwrap());
        self.h.try_send(conn_id, data).unwrap();
    }
}

#[allow(unreachable_code)]
#[throws(Error)]
fn main() {
    env_logger::init();
    let _ = remove_file("/tmp/shaman.sock");
    let mut server = ShamanServer::new("/tmp/shaman.sock")?;
    let handler = Handler { h: server.handle() };
    server.set_message_handler(handler);

    let serverhandle = server.handle();

    server.spawn();

    loop {
        let now = Instant::now();
        let data = format!("{:?}", now);
        serverhandle.try_broadcast(data.as_bytes())?;
        sleep(Duration::from_millis(1000));
    }
}
