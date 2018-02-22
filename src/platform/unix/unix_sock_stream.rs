extern crate libc; 
extern crate serde;
extern crate rmp_serde;
extern crate futures;
extern crate tokio_io;

use serde::{ Serialize, de, Deserialize };
use rmp_serde::{ Serializer, Deserializer };
use std::marker::PhantomData;
use std::io::{ Result, Error, Read, Write, ErrorKind };
use std::cell::Cell;
use std::ptr::null_mut;
use libc::*;
use super::super::super::{ Sender, Receiver };

use futures::prelude::*;
use tokio_io::{ AsyncWrite, AsyncRead };

struct FError<T> {
    _phantom: PhantomData<T>
}

impl<T> Future for FError<T> {
    type Item = T;
    type Error = Error;
    fn poll(&mut self) -> Poll<T, Error> {
        Err(Error::last_os_error())
    }
}

pub struct UnixSockStreamServer<T> {
    connect_socket:     i32,
    socket:             Cell<Option<i32>>,
    _phantom:           PhantomData<T>
}
impl<T> UnixSockStreamServer<T> {
    pub fn new<S: AsRef<str>>(name: S) -> Result<UnixSockStreamServer<T>> {
        use std::ptr::copy_nonoverlapping;
        use std::io::ErrorKind;
        let name : &str = name.as_ref();
        if name.as_bytes().len() > 106 {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        let mut sock = sockaddr_un { 
            sun_family:     AF_UNIX as u16, 
            sun_path:       [0; 108]
        }; 
        unsafe { copy_nonoverlapping(name.as_ptr() as *const i8, &mut sock.sun_path[1], name.len()) };
        let socket = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if socket == -1 {
            return Err(Error::last_os_error());
        }
        if unsafe { bind(socket, &sock as *const sockaddr_un as *const sockaddr, 4 + name.len() as u32) } != 0 || unsafe { listen(socket, 1) } != 0 { 
            return Err(Error::last_os_error());
        }
        Ok(UnixSockStreamServer {
            connect_socket: socket,
            socket:         Cell::new(None),
            _phantom:       PhantomData
        })
    }
} 

struct ARecvServer<'a, T: 'a> {
    u_sock: &'a UnixSockStreamServer<T>
}
impl<'a, T> Read for ARecvServer<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> { 
        use std::{ mem, ptr };
        let mut size = 0usize;
        let socket = self.u_sock.socket.get().ok_or(Error::from(ErrorKind::InvalidData))?;
        if unsafe { read(socket, &mut size as *mut usize as *mut c_void, mem::size_of::<usize>()) } != mem::size_of::<usize>() as isize {
            return Err(Error::last_os_error());
        }
        let mut buff = vec![0u8; size];
        if unsafe { read(socket, buff.as_mut_ptr() as *mut c_void, size) } != size as isize {
            return Err(Error::last_os_error());
        }
        if buf.len() < size {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        unsafe { ptr::copy_nonoverlapping(&buff[..] as *const [u8] as *const u8, buf as *mut [u8] as *mut u8, size) };
        return Ok(size + mem::size_of::<usize>())
    }
}
impl<'a, T> AsyncRead for ARecvServer<'a, T> { }

impl<'a, T> Future for ARecvServer<'a, T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        use std::mem::zeroed;
        let socket = self.u_sock.socket.get().ok_or(Error::from(ErrorKind::InvalidData))?;
        let mut poll_s = pollfd {
            fd:         socket, 
            events:     POLLIN,
            revents:    unsafe { zeroed() }
        };
        let r = unsafe { poll(&mut poll_s, 1, 0) };
        if r < 0 {
            return Err(Error::last_os_error());
        } 
        if r == 0 {
            return Ok(Async::NotReady);
        }
        if poll_s.revents & POLLIN > 0 {
            let mut buf = vec![0u8; 65536];
            match self.read_buf(&mut buf) {
                Ok(Async::Ready(t)) => { 
                    return Ok(Async::Ready(Deserialize::deserialize(&mut Deserializer::new(&buf[buf.len()-t..])).unwrap()))
                },
                Ok(Async::NotReady) => {
                    return Ok(Async::NotReady)
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
        return Ok(Async::NotReady);
    }
}

impl<T> Receiver for UnixSockStreamServer<T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a> {
        let sock = self.socket.get();
        if sock == None { 
            let s = unsafe { accept4(self.connect_socket, null_mut(), null_mut(), SOCK_NONBLOCK) };
            if s < 0 {
                return Box::new(FError { _phantom: PhantomData });
            }
            self.socket.set(Some(s));
        } 
        Box::new(ARecvServer {
            u_sock: self
        })
    }
}

struct ASendServer<'a, T: 'a> {
    u_sock: &'a UnixSockStreamServer<T>
}
impl <'a, T> Write for ASendServer<'a, T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        use std::mem;
        let socket = self.u_sock.socket.get().ok_or(Error::from(ErrorKind::InvalidData))?;
        if unsafe { write(socket, &buf.len() as *const usize as *const c_void, mem::size_of::<usize>()) } == 0 {
            return Err(Error::last_os_error());
        }
        if unsafe { write(socket, buf.as_ptr() as *const c_void, buf.len()) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(buf.len() + mem::size_of::<usize>())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
impl<'a, T> AsyncWrite for ASendServer<'a, T> {
    fn shutdown(&mut self) -> Poll<(), Error> {
        let socket = self.u_sock.socket.get().ok_or(Error::from(ErrorKind::InvalidData))?;
        if unsafe { shutdown(socket, SHUT_RDWR) } < 0 {
            if unsafe { close(socket) } < 0 {
                return Err(Error::last_os_error());
            }
            return Ok(Async::Ready(()));
        }
        Ok(Async::Ready(()))
    }
} 
impl<T> Sender for UnixSockStreamServer<T> 
    where T: serde::Serialize 
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a> { 
        use tokio_io::io::write_all;
        let sock = self.socket.get();
        if sock == None { 
            let s = unsafe { accept4(self.connect_socket, null_mut(), null_mut(), SOCK_NONBLOCK) };
            if s < 0 {
                return Box::new(FError { _phantom: PhantomData });
            }
            self.socket.set(Some(s));
        } 
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        Box::new(write_all(ASendServer {
            u_sock:     self,
        }, buf).map(|_| ()))
    }
}

pub struct UnixSockStreamClient<T> {
    socket:             i32,
    _phantom:           PhantomData<T>
}
impl<T> UnixSockStreamClient<T> 
    where T: de::DeserializeOwned + Serialize
{
    pub fn new<S: AsRef<str>>(name: S) -> Result<UnixSockStreamClient<T>> { 
        use std::ptr::copy_nonoverlapping;
        use std::io::ErrorKind;
        let name : &str = name.as_ref();
        if name.as_bytes().len() > 106 {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        let mut sock = sockaddr_un { 
            sun_family:     AF_UNIX as u16, 
            sun_path:       [0; 108]
        }; 
        unsafe { copy_nonoverlapping(name.as_ptr() as *const i8, &mut sock.sun_path[1], name.len()) };
        let socket = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if socket == -1 {
            return Err(Error::last_os_error());
        }
        if unsafe { connect(socket, &sock as *const sockaddr_un as *const sockaddr, 4 + name.len() as u32) } != 0 {
            return Err(Error::last_os_error());
        }
        Ok(UnixSockStreamClient {
            socket:             socket,
            _phantom:           PhantomData
        }) 
    }
}

struct ARecvClient<'a, T: 'a> {
    u_sock: &'a UnixSockStreamClient<T>
}
impl<'a, T> Read for ARecvClient<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> { 
        use std::{ mem, ptr };
        let mut size = 0usize;
        let socket = self.u_sock.socket;
        if unsafe { read(socket, &mut size as *mut usize as *mut c_void, mem::size_of::<usize>()) } != mem::size_of::<usize>() as isize {
            return Err(Error::last_os_error());
        }
        let mut buff = vec![0u8; size];
        if unsafe { read(socket, buff.as_mut_ptr() as *mut c_void, size) } != size as isize {
            return Err(Error::last_os_error());
        }
        if buf.len() < size {
            return Err(Error::from(ErrorKind::InvalidInput));
        }
        unsafe { ptr::copy_nonoverlapping(&buff[..] as *const [u8] as *const u8, buf as *mut [u8] as *mut u8, size) };
        return Ok(size + mem::size_of::<usize>())
    }
}
impl<'a, T> AsyncRead for ARecvClient<'a, T> { }

impl<'a, T> Future for ARecvClient<'a, T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        use std::mem::zeroed;
        let socket = self.u_sock.socket;
        let mut poll_s = pollfd {
            fd:         socket, 
            events:     POLLIN,
            revents:    unsafe { zeroed() }
        };
        let r = unsafe { poll(&mut poll_s, 1, 0) };
        if r < 0 {
            return Err(Error::last_os_error());
        } 
        if r == 0 {
            return Ok(Async::NotReady);
        }
        if poll_s.revents & POLLIN > 0 {
            let mut buf = vec![0u8; 65536];
            match self.read_buf(&mut buf) {
                Ok(Async::Ready(t)) => { 
                    return Ok(Async::Ready(Deserialize::deserialize(&mut Deserializer::new(&buf[buf.len()-t..])).unwrap()))
                },
                Ok(Async::NotReady) => {
                    return Ok(Async::NotReady)
                },
                Err(e) => {
                    return Err(e)
                }
            }
        }
        return Ok(Async::NotReady);
    }
}

impl<T> Receiver for UnixSockStreamClient<T> 
    where T: serde::de::DeserializeOwned
{
    type Item = T;
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a> {
        Box::new(ARecvClient {
            u_sock: self
        })
    }
}

struct ASendClient<'a, T: 'a> {
    u_sock: &'a UnixSockStreamClient<T>
}
impl <'a, T> Write for ASendClient<'a, T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        use std::mem;
        let socket = self.u_sock.socket;
        if unsafe { write(socket, &buf.len() as *const usize as *const c_void, mem::size_of::<usize>()) } == 0 {
            return Err(Error::last_os_error());
        }
        if unsafe { write(socket, buf.as_ptr() as *const c_void, buf.len()) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(buf.len() + mem::size_of::<usize>())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
impl<'a, T> AsyncWrite for ASendClient<'a, T> {
    fn shutdown(&mut self) -> Poll<(), Error> {
        let socket = self.u_sock.socket;
        if unsafe { shutdown(socket, SHUT_RDWR) } < 0 {
            if unsafe { close(socket) } < 0 {
                return Err(Error::last_os_error());
            }
            return Ok(Async::Ready(()));
        }
        Ok(Async::Ready(()))
    }
} 
impl<T> Sender for UnixSockStreamClient<T> 
    where T: serde::Serialize 
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a> { 
        use tokio_io::io::write_all;
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        Box::new(write_all(ASendClient {
            u_sock:     self,
        }, buf).map(|_| ()))
    }
} 
