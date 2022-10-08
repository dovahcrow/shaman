use crate::{errors::ShamanError, SIZE};
use fehler::{throw, throws};
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::collections::VecDeque;

const SIZEM1: usize = SIZE - 1;

pub struct Duplex {
    pub(crate) tx: IPCSender<u8>,
    pub(crate) rx: IPCReceiver<u8>,
    tx_buf: VecDeque<u8>,
    rx_buf: Vec<u8>,
}

impl Duplex {
    pub fn new(tx: IPCSender<u8>, rx: IPCReceiver<u8>) -> Self {
        Duplex {
            tx,
            rx,
            tx_buf: VecDeque::new(),
            rx_buf: Vec::new(),
        }
    }

    #[throws(ShamanError)]
    pub fn flush(&mut self) {
        let mut remaining = 1;
        while remaining != 0 && !self.tx_buf.is_empty() {
            let status = unsafe {
                self.tx
                    .send_trusted(|buf| write_vec_deque_and_trunc(buf, &mut self.tx_buf))?
            };
            remaining = status.remaining;
        }
    }

    #[throws(ShamanError)]
    pub fn send(&mut self, data: &[u8]) {
        // flush the pending data first
        self.flush()?;

        if !self.tx_buf.is_empty() {
            // the IPCSender is full, we send no more
            throw!(ShamanError::SenderFull);
        }

        unsafe {
            self.tx.send_trusted(|mut buf| {
                let orig_len = buf.len();

                let len_data = data.len().to_ne_bytes();
                let n = write_slice(buf, &len_data);
                buf = &mut buf[n..];
                self.tx_buf.extend(&len_data[n..]);

                let n = write_slice(buf, &data);
                buf = &mut buf[n..];
                self.tx_buf.extend(&data[n..]);

                orig_len - buf.len()
            })?
        };
    }

    // returns data with the length
    #[throws(ShamanError)]
    pub fn try_recv_with<F, R>(&mut self, mut f: F) -> Option<R>
    where
        F: FnMut(&[u8]) -> R,
    {
        let mut ret = None;
        let mut remaining = 1;
        while ret.is_none() && remaining != 0 {
            let status = unsafe {
                match self.rx_buf.len() {
                    0 => self.rx.receive_trusted(|data| {
                        if data.len() < SIZE {
                            self.rx_buf.extend(data);
                            return data.len();
                        }

                        let len = usize::from_ne_bytes(data[..SIZE].try_into().unwrap());
                        if data.len() < SIZE + len {
                            self.rx_buf.extend(data);
                            return data.len();
                        }
                        ret = Some(f(&data[SIZE..SIZE + len]));

                        SIZE + len
                    })?,
                    1..=SIZEM1 => self.rx.receive_trusted(|data| {
                        let n = data.len().min(SIZE - self.rx_buf.len());
                        self.rx_buf.extend(&data[..n]);
                        n
                    })?,
                    n => {
                        let len = usize::from_ne_bytes(self.rx_buf[..SIZE].try_into().unwrap());
                        let remaining_len = len - (n - SIZE);
                        let status = self.rx.receive_trusted(|data| {
                            let n = data.len().min(remaining_len);
                            self.rx_buf.extend(&data[..n]);
                            n
                        })?;

                        assert!(self.rx_buf.len() <= len + SIZE);
                        if self.rx_buf.len() == len + SIZE {
                            ret = Some(f(&self.rx_buf[SIZE..]));
                            self.rx_buf.clear();
                        }

                        status
                    }
                }
            };

            remaining = status.remaining;
        }

        ret
    }

    // returns data with the length
    #[throws(ShamanError)]
    pub fn try_recv(&mut self) -> Option<Vec<u8>> {
        self.try_recv_with(|data| data.to_vec())?
    }
}

fn write_vec_deque_and_trunc(buf: &mut [u8], vd: &mut VecDeque<u8>) -> usize {
    let (buf1, buf2) = vd.as_slices();

    let n = buf.len().min(buf1.len());
    buf[..n].copy_from_slice(&buf1[..n]);

    // buf.len().saturating_sub(n) is how many bytes that can be still written into buf
    let m = buf.len().saturating_sub(n).min(buf2.len());
    buf[n..n + m].copy_from_slice(&buf2[..m]);

    drop(vd.drain(..n + m));

    n + m
}

fn write_slice(buf: &mut [u8], slice: &[u8]) -> usize {
    let n = buf.len().min(slice.len());
    buf[..n].copy_from_slice(&slice[..n]);
    n
}
