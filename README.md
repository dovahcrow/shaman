# Shaman

A Rust shared memory IPC library.

Shaman is designed for efficient IPC between server/client processes on a single machine. 
Shaman supports doing request/response or subscription between one server process and multiple
client processes through shared memory.

Shaman first exchanges the shared memory fd, which is created using [shmem-ipc](https://github.com/diwic/shmem-ipc/) 
through a UNIX domain socket. After that, the client and the server communicates with each other through the shared memory.
Shaman exposes standard `mpsc::{Sender<Response>, Receiver<Request>}` for implementing the server logic and 
`mpsc::{Sender<Request>, Receiver<Response>}` for implementing the client logic.

## Usage

Please take a look at the [examples](examples) folder for a clock server example.
