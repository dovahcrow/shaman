# Shaman

A Rust shared memory IPC library.

Shaman is designed for efficient IPC between server/client processes on a single machine using shared memory. 

Shaman first exchanges the shared memory fd, which is created using [shmem-ipc](https://github.com/diwic/shmem-ipc/) 
through a UNIX domain socket. After that, the client and the server communicates with each other using a
channel-like interface.

## Usage
Please take a look at the [examples](examples) folder for a clock server example.
