use crate::{errors::ShamanError, SIZE};
use fehler::throws;
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::{collections::VecDeque, mem::swap};

pub struct Duplex {
    pub(crate) tx: IPCSender<u8>,
    pub(crate) rx: IPCReceiver<u8>,
    tx_buf: VecDeque<u8>,
    rx_buf: VecDeque<u8>,
}

impl Duplex {
    pub fn new(tx: IPCSender<u8>, rx: IPCReceiver<u8>) -> Self {
        Duplex {
            tx,
            rx,
            tx_buf: VecDeque::new(),
            rx_buf: VecDeque::new(),
        }
    }

    #[throws(ShamanError)]
    pub fn flush(&mut self) {
        let mut remaining = 1;
        while remaining != 0 && !self.tx_buf.is_empty() {
            let status = unsafe {
                self.tx
                    .send_trusted(|buf| write_vec_deque(buf, &mut self.tx_buf))?
            };
            remaining = status.remaining;
        }
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, data: &[u8]) {
        unsafe {
            self.tx.send_trusted(|mut buf| {
                let orig_len = buf.len();

                let n = write_vec_deque(buf, &mut self.tx_buf);
                buf = &mut buf[n..];

                if !buf.is_empty() {
                    let len_data = data.len().to_ne_bytes();
                    let n = write_slice(buf, &len_data);
                    buf = &mut buf[n..];

                    self.tx_buf.extend(&len_data[n..]);
                }

                if !buf.is_empty() {
                    let n = write_slice(buf, &data);
                    buf = &mut buf[n..];

                    self.tx_buf.extend(&data[n..]);
                }

                orig_len - buf.len()
            })?
        };

        self.flush()?
    }

    #[throws(ShamanError)]
    pub fn recv(&mut self) {
        let mut remaining = 1;
        while remaining != 0 {
            let status = unsafe {
                self.rx.receive_trusted(|data| {
                    self.rx_buf.extend(data);
                    data.len()
                })?
            };
            remaining = status.remaining;
        }
    }

    // returns data with the length
    #[throws(ShamanError)]
    pub fn decode(&mut self) -> Option<Vec<u8>> {
        if self.rx_buf.len() < SIZE {
            return None;
        }
        let mut n = [0; SIZE];
        let mut iter = self.rx_buf.iter();
        for i in 0..SIZE {
            n[i] = *iter.next().unwrap();
        }
        let n = usize::from_ne_bytes(n);

        if self.rx_buf.len() < n + SIZE {
            return None;
        }

        let mut buf = self.rx_buf.split_off(n + SIZE);
        swap(&mut buf, &mut self.rx_buf);
        Some(Vec::from_iter(buf))
    }
}

fn write_vec_deque(buf: &mut [u8], vecdeque: &mut VecDeque<u8>) -> usize {
    let n = buf.len().min(vecdeque.len());
    let new_buf = vecdeque.split_off(n);
    let (buf1, buf2) = vecdeque.as_slices();
    buf[..buf1.len()].copy_from_slice(&buf1);
    buf[buf1.len()..buf1.len() + buf2.len()].copy_from_slice(&buf2);
    *vecdeque = new_buf;

    n
}

fn write_slice(buf: &mut [u8], slice: &[u8]) -> usize {
    let n = buf.len().min(slice.len());
    buf[..n].copy_from_slice(&slice[..n]);

    n
}
