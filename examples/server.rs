use std::{
    fs::remove_file,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::Error;
use fehler::throws;
use shaman::{Request, RequestHandler, Response, ShamanServer, ShamanServerHandle};

struct Handler;

impl RequestHandler for Handler {
    fn handle(&mut self, conn_id: usize, req: Request, handle: &ShamanServerHandle) {
        let _ = handle.send((
            Some(conn_id),
            Response::Success {
                id: req.id,
                data: req.params,
            },
        ));
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
        handle.send((
            None,
            Response::Subscription {
                channel: "Time".into(),
                data,
            },
        ))?;

        sleep(Duration::from_secs(1));
    }
}
