use std::{
    fs::remove_file,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::Error;
use fehler::throws;
use shaman::{Response, ShamanServer};

#[allow(unreachable_code)]
#[throws(Error)]
fn main() {
    env_logger::init();
    let _ = remove_file("/tmp/shaman.sock");
    let (server, handle) = ShamanServer::new("/tmp/shaman.sock")?;

    server.spawn();

    loop {
        if let Ok((conn_id, req)) = handle.rx.try_recv() {
            handle.tx.send((
                Some(conn_id),
                Response::Success {
                    id: req.id,
                    data: req.params,
                },
            ))?
        }

        let now = Instant::now();
        let data = format!("{:?}", now).as_bytes().to_vec();
        handle.tx.send((
            None,
            Response::Subscription {
                channel: "Time".into(),
                data,
            },
        ))?;

        sleep(Duration::from_secs(1));
    }
}
