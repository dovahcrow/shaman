mod shared;

use anyhow::Error;
use fehler::throws;
use shaman::{MessageHandler, ShamanServer, ShamanServerSendHandle};
use shared::{Request, Response};
use std::{
    fs::remove_file,
    thread::sleep,
    time::{Duration, Instant},
};

struct Handler;

impl MessageHandler for Handler {
    fn on_data(&mut self, data: &[u8], handle: &mut ShamanServerSendHandle) {
        let req: Request = bincode::deserialize(data).unwrap();
        let _ = handle.try_send(
            &bincode::serialize(&Response::Success {
                id: req.id,
                data: req.params,
            })
            .unwrap(),
        );
    }
}

#[allow(unreachable_code)]
#[throws(Error)]
fn main() {
    env_logger::init();
    let _ = remove_file("/tmp/shaman.sock");
    let (server, handle) = ShamanServer::new("/tmp/shaman.sock", Handler)?;

    server.spawn();

    loop {
        let now = Instant::now();
        let data = format!("{:?}", now).as_bytes().to_vec();
        handle.try_broadcast(&bincode::serialize(&Response::Subscription {
            channel: "Time".into(),
            data,
        })?)?;

        sleep(Duration::from_millis(1));
    }
}
