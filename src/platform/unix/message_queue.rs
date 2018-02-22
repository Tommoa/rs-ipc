extern crate serde;
extern crate rmp_serde;
extern crate tokio_io;
extern crate futures;
use libc::{ c_char, mq_open, mq_getattr, mq_receive, mq_close, mq_send, mq_unlink, mq_attr, mqd_t };
use libc::{ O_RDWR, O_CREAT, O_NONBLOCK, S_IWUSR, S_IRUSR, S_IROTH, S_IWOTH };
use std::marker::PhantomData;
use std::mem::zeroed;
use std::ffi::CString;
use std::io::{ Result, Error };

use serde::Deserialize;
use rmp_serde::{ Deserializer, Serializer };
use super::super::super::{ Sender, Receiver };
use tokio_io::{ AsyncRead, AsyncWrite };
use futures::prelude::*;
use std::io::{ Read, Write };

/// A POSIX message queue implementation
///
/// This is a one-way ipc protocol, the queuing process can read and immediately get the value that
/// it just queued. If you want a two-way IPC method, use unix sockets.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MessageQueue<T> {
    mq_name:    CString,
    fd:         mqd_t,
    size:       usize,
    _phantom:   PhantomData<T>,
}
impl<T> MessageQueue<T> 
    where T: serde::de::DeserializeOwned
{
    pub fn new<S: AsRef<str>>(name: S) -> Result<MessageQueue<T>> {
        let mut n = String::from("/");
        n.push_str(name.as_ref());
        let name = CString::new(n).unwrap();
        let fd = unsafe { mq_open(name.as_ptr(), O_NONBLOCK | O_RDWR | O_CREAT, S_IWUSR | S_IRUSR | S_IROTH | S_IWOTH, 0i64) }; 
        if fd == -1 { 
            return Err(Error::last_os_error());
        }
        let mut attr : mq_attr = unsafe { zeroed() };
        if unsafe { mq_getattr(fd, &mut attr) } < 0 {
            return Err(Error::last_os_error());
        } 
        Ok(MessageQueue {
            mq_name:    name,
            fd:         fd,
            size:       attr.mq_msgsize as usize,
            _phantom:   PhantomData
        })
    }
}

struct AWrite<'a, T: 'a> {
    m_queue:    &'a MessageQueue<T>,
}
impl <'a, T> Write for AWrite<'a, T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if unsafe {
            mq_send(self.m_queue.fd, buf.as_ptr() as *const i8, buf.len(), 0u32)
        }== -1 { 
            return Err(Error::last_os_error());
        }
        Ok(buf.len())
    }
    /// This does nothing for a message queue.
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
impl <'a, T> AsyncWrite for AWrite<'a, T> {
    fn shutdown(&mut self) -> Poll<(), Error> { 
        if unsafe { mq_unlink(self.m_queue.mq_name.as_ptr()) } < 0 {
            if unsafe { mq_close(self.m_queue.fd) } < 0 {
                return Err(Error::last_os_error());
            }
            return Ok(Async::Ready(()));
        }
        Ok(Async::Ready(()))
    }
}
impl<'a, T> Future for AWrite<'a, T> 
    where T: serde::Serialize
{
    type Item = ();
    type Error = Error;
    
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> { 
        let mut attr : mq_attr = unsafe { zeroed() };
        if unsafe { mq_getattr(self.m_queue.fd, &mut attr) } < 0 {
            return Err(Error::last_os_error());
        } 
        if attr.mq_maxmsg == attr.mq_curmsgs {
            return Ok(Async::NotReady);
        }
        Ok(Async::Ready(()))
    }
}
impl<T> Sender for MessageQueue<T>
    where T: serde::Serialize
{
    type Item = T;
    fn send(&self, to_send: T) -> Result<()> {
        self.async_send(to_send).wait()
    }
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a> {
        use tokio_io::io::write_all;
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        Box::new(write_all(AWrite { m_queue: self }, buf).map(|_| ()))
    }
}

struct ARead<'a, T: 'a> {
    m_queue:    &'a MessageQueue<T>,
}

impl<'a, T> Read for ARead<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        use std::ptr::null_mut;
        let len = unsafe {
            mq_receive(self.m_queue.fd, buf.as_mut_ptr() as *mut c_char, self.m_queue.size, null_mut())
        };
        if len < 0 {
            return Err(Error::last_os_error());
        }
        Ok(len as usize)
    }
}

impl<'a, T> AsyncRead for ARead<'a, T> {}

impl<'a, T> Future for ARead<'a, T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    type Error = Error;
    
    fn poll(&mut self) -> Poll<Self::Item, Self::Error> { 
        let mut buf = vec![0u8; self.m_queue.size];
        match self.read_buf(&mut buf) {
            Ok(Async::Ready(t)) => { 
                Ok(Async::Ready(Deserialize::deserialize(&mut Deserializer::new(&buf[buf.len()-t..])).unwrap()))
            },
            Ok(Async::NotReady) => {
                Ok(Async::NotReady)
            },
            Err(e) => {
                Err(e)
            }
        }
    }
}

impl<T> Receiver for MessageQueue<T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    fn recv(&self) -> Result<T> { 
        self.async_recv().wait()
    } 
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a> {
        Box::new(ARead {
            m_queue:    self
        })
    }
}

impl<T> Drop for MessageQueue<T> {
    fn drop(&mut self) {
        if unsafe { mq_unlink(self.mq_name.as_ptr()) } < 0 {
            unsafe { mq_close(self.fd) };
        }
    }
} 
