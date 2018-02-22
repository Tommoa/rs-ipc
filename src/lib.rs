#![feature(proc_macro, conservative_impl_trait, generators)]

extern crate libc;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate rmp_serde;
extern crate tokio_io;
extern crate futures;

#[cfg(windows)]
extern crate winapi;
#[cfg(windows)]
extern crate bytes;

mod platform;
pub use platform::*;


use std::io::{ Error, Result };
use futures::Future;
/// Receives a deserializable object
pub trait Receiver {
    type Item;
    /// Synchronous receive
    fn recv(&self) -> Result<Self::Item> {
        self.async_recv().wait()
    }
    /// Asynchronous receive
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a>;
}
/// Sends a serializable object
pub trait Sender {
    type Item;
    /// Synchronous send
    fn send(&self, message: Self::Item) -> Result<()> {
        self.async_send(message).wait()
    }
    /// Asynchronous send
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a>;
}

#[cfg(test)]
#[cfg(unix)]
mod test {
    use super::*;
    use rand::random;
    #[test]
    fn one_way_basic() {
        use super::message_queue::*;
        let (tx, rx) = MessageQueue::<i32>::channel().unwrap();
        let r = rx.async_recv();
        tx.send(2019).unwrap();
        assert_eq!(2019, r.wait().unwrap());
    }
    #[test]
    fn one_way_tx_thread() { 
        use std::thread;
        use super::message_queue::*;
        let i = random();
        let (tx, rx) = MessageQueue::<i32>::channel().unwrap();
        let c1 = thread::spawn(move || -> i32 {
            tx.send(i).unwrap();
            i
        });
        let i = rx.recv().unwrap();
        assert_eq!(c1.join().unwrap(), i);
    }
    #[test]
    fn one_way_rx_thread() {
        use std::thread;
        use super::message_queue::*;
        let i = random();
        let (tx, rx) = MessageQueue::<i32>::channel().unwrap();
        let c1 = thread::spawn(move || -> i32 {
            rx.recv().unwrap()
        });
        tx.send(i).unwrap();
        assert_eq!(c1.join().unwrap(), i);
    }
    #[test]
    fn one_way_multi_thread() {
        use std::thread;
        use super::message_queue::*;
        let i = random();
        let (tx, rx) = MessageQueue::<i32>::channel().unwrap();
        let c1 = thread::spawn(move || -> i32 {
            rx.recv().unwrap()
        });
        let c2 = thread::spawn(move || -> i32 {
            tx.send(i).unwrap();
            i
        });
        assert_eq!(c2.join().unwrap(), c1.join().unwrap());
    }
    #[test]
    #[cfg(unix)] // Requires Windows > 17061
    fn af_unix_stream() {
        use super::unix_sock_stream::*;
        let tx = UnixSockStreamServer::<i32>::new("test").unwrap();
        let rx = UnixSockStreamClient::<i32>::new("test").unwrap();
        println!("here");
        tx.send(4).unwrap();
        println!("sent");
        assert_eq!(4, rx.recv().unwrap());
        println!("recved");
        rx.send(5).unwrap();
        assert_eq!(5, tx.recv().unwrap());
    }
    #[test]
    fn af_unix_seq() {
        use super::unix_sock_seqpacket::*;
        let tx = UnixSockSeqPacketServer::<i32>::new("test").unwrap();
        let rx = UnixSockSeqPacketClient::<i32>::new("test").unwrap();
        println!("here");
        tx.send(4).unwrap();
        println!("sent");
        assert_eq!(4, rx.recv().unwrap());
        println!("recved");
        rx.send(5).unwrap();
        assert_eq!(5, tx.recv().unwrap());
    }
}
#[cfg(test)]
#[cfg(windows)]
mod test {
    use super::*;
    use rand::random;
    #[test]
    fn one_way_basic() {
        use super::one_way::*;
        let (tx, rx) = (NamedPipeServer::<i32>::new("hello").unwrap(), NamedPipeClient::<i32>::new("hello").unwrap());
        tx.send(2019).unwrap();
        assert_eq!(2019, rx.recv().unwrap()); 
        rx.send(2019).unwrap(); 
        assert_eq!(2019, tx.recv().unwrap());
    }
}
