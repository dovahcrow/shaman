use crate::{
    errors::ShamanError::{self, *},
    SIZE,
};
use fehler::{throw, throws};
use shmem_ipc::sharedring::{Receiver as IPCReceiver, Sender as IPCSender};
use std::collections::VecDeque;
use std::fs::File;

const SIZEM1: usize = SIZE - 1;

/// Duplex only has nonblocking API
/// Duplex build a framed message format on top of shmem channel.
/// A framed message is a Vec<u8> with first 8 bytes describing how long the message is (exclude the first 8 bytes).
/// For send, if the channel has enough space, directly write the length then the data to the channel.
/// If the channel has no enough space, write as much as possible to the channel and rest to the tx_buf.
/// Consecutive sends will first try to flush the tx_buf to the channel before send any new message.
/// For recv, if the channel has a complete message, directly receive it. Otherwise receive whatever is available
/// to the rx_buf.
pub struct Duplex {
    pub(crate) tx: IPCSender<u8>,
    pub(crate) rx: IPCReceiver<u8>,
    pub(crate) tx_buf: VecDeque<u8>,
    pub(crate) rx_buf: Vec<u8>,
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
    pub fn flush(tx: &mut IPCSender<u8>, tx_buf: &mut VecDeque<u8>) {
        let mut remaining = 1;
        while remaining != 0 && !tx_buf.is_empty() {
            let status = unsafe { tx.send_trusted(|buf| write_vec_deque_and_trunc(buf, tx_buf))? };
            remaining = status.remaining;
        }
    }

    #[throws(ShamanError)]
    pub fn try_send(tx: &mut IPCSender<u8>, tx_buf: &mut VecDeque<u8>, data: &[u8]) {
        // flush the pending data first
        Self::flush(tx, tx_buf)?;

        if !tx_buf.is_empty() {
            // the IPCSender is full, we send no more
            throw!(ShamanError::WouldBlock);
        }

        unsafe {
            tx.send_trusted(|mut buf| {
                let orig_len = buf.len();

                let len_data = data.len().to_ne_bytes();
                let n = write_slice(buf, &len_data);
                buf = &mut buf[n..];
                tx_buf.extend(&len_data[n..]);

                let n = write_slice(buf, &data);
                buf = &mut buf[n..];
                tx_buf.extend(&data[n..]);

                orig_len - buf.len()
            })?;
        };

        Self::flush(tx, tx_buf)?;
    }

    #[throws(ShamanError)]
    pub fn try_recv_with<F, R>(rx: &mut IPCReceiver<u8>, rx_buf: &mut Vec<u8>, mut f: F) -> R
    where
        F: FnMut(&[u8]) -> R,
    {
        let mut ret = None;
        let mut remaining = 1;
        while ret.is_none() && remaining != 0 {
            let status = unsafe {
                match rx_buf.len() {
                    0 => rx.receive_trusted(|data| {
                        if data.len() < SIZE {
                            rx_buf.extend(data);
                            return data.len();
                        }

                        let len = usize::from_ne_bytes(data[..SIZE].try_into().unwrap());
                        if data.len() < SIZE + len {
                            rx_buf.extend(data);
                            return data.len();
                        }
                        ret = Some(f(&data[SIZE..SIZE + len]));

                        SIZE + len
                    })?,
                    1..=SIZEM1 => rx.receive_trusted(|data| {
                        let n = data.len().min(SIZE - rx_buf.len());
                        rx_buf.extend(&data[..n]);
                        n
                    })?,
                    n => {
                        let len = usize::from_ne_bytes(rx_buf[..SIZE].try_into().unwrap());
                        let remaining_len = len - (n - SIZE);
                        let status = rx.receive_trusted(|data| {
                            let n = data.len().min(remaining_len);
                            rx_buf.extend(&data[..n]);
                            n
                        })?;

                        assert!(rx_buf.len() <= len + SIZE);
                        if rx_buf.len() == len + SIZE {
                            ret = Some(f(&rx_buf[SIZE..]));
                            rx_buf.clear();
                        }

                        status
                    }
                }
            };

            remaining = status.remaining;
        }

        ret.ok_or(WouldBlock)?
    }

    #[throws(ShamanError)]
    pub fn try_recv(rx: &mut IPCReceiver<u8>, rx_buf: &mut Vec<u8>) -> Vec<u8> {
        Self::try_recv_with(rx, rx_buf, |data| data.to_vec())?
    }

    pub fn send_notifier(&self) -> &File {
        self.tx.full_signal()
    }

    pub fn recv_notifier(&self) -> &File {
        self.rx.empty_signal()
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
