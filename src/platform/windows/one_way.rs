extern crate winapi;
extern crate serde;
extern crate rmp_serde;
extern crate futures;
extern crate tokio_io;
extern crate bytes;

use winapi::um::namedpipeapi::*;
use winapi::um::minwinbase::OVERLAPPED;
use winapi::um::ioapiset::GetOverlappedResult;
use winapi::um::fileapi::{ CreateFileW, WriteFile, ReadFile };
use winapi::um::handleapi::{ CloseHandle };
use winapi::shared::ntdef::HANDLE;
use winapi::ctypes::c_void;

use std::io::{ Result, Error, Write, Read };
use std::ffi::{ OsStr, OsString };
use std::{ mem };

use std::marker::PhantomData;
use std::ptr::null_mut;
use std::os::windows::prelude::*;
use std::cell::Cell;

use serde::{ Serialize, Deserialize, de };
use rmp_serde::{ Serializer, Deserializer };
use tokio_io::{ AsyncRead, AsyncWrite };
use futures::prelude::*;
use super::super::super::{ Sender, Receiver };
use bytes::Buf;

struct FError;

impl Future for FError {
    type Item = ();
    type Error = Error;
    fn poll(&mut self) -> Poll<(), Error> {
        Err(Error::last_os_error())
    }
}

struct ServerAsyncWrite<'a, T: 'a> {
    np:     &'a NamedPipeServer<T>,
    ovr:    OVERLAPPED
}
impl<'a, T> AsyncWrite for ServerAsyncWrite<'a, T> {
    fn shutdown(&mut self) -> Poll<(), Error> {
        if unsafe { DisconnectNamedPipe(self.np.handle as HANDLE) } == 0 {
            return Err(Error::last_os_error());
        }
        if unsafe { CloseHandle(self.np.handle as HANDLE) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Async::Ready(()))
    }
    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, Error> {
        if !buf.has_remaining() {
            return Ok(Async::Ready(0));
        }
        if self.ovr.hEvent as usize == 0 {
            match self.write(buf.bytes()) {
                Err(e) => {
                    if e.raw_os_error() != Some(997) {
                        return Err(e);
                    }
                },
                Ok(x) => {
                    return Ok(Async::Ready(x));
                }
            }
        } 
        // We have already told the OS to write
        let mut n_bytes = 0;
        if unsafe { GetOverlappedResult(self.np.handle as HANDLE, &mut self.ovr, &mut n_bytes, 0) } == 0 {
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(996) {
                return Err(e);
            }
            return Ok(Async::NotReady);
        }
        Ok(Async::Ready(n_bytes as usize))
    }
}
impl<'a, T> Write for ServerAsyncWrite<'a, T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let handle = self.np.handle;
        let mut n_bytes = 0;
        if unsafe{ WriteFile(handle as HANDLE, buf.as_ptr() as *const c_void, buf.len() as u32, &mut n_bytes, &mut self.ovr) } == 0 {
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(997) {
                return Err(e);
            }
        }
        Ok(n_bytes as usize)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl<T> Sender for NamedPipeServer<T> 
    where T: serde::Serialize 
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a> { 
        use tokio_io::io::write_all;
        use std::mem::zeroed;
        if let Some(ovr) = self.ovr.get() {
            let mut ovr = unsafe { mem::transmute(ovr) };
            let mut s = 0;
            if unsafe {
                GetOverlappedResult(self.handle as HANDLE, &mut ovr, &mut s, 1)
            } == 0 {
                return Box::new(FError);
            }
            self.ovr.set(None);
        }
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        Box::new(write_all(ServerAsyncWrite {
            np:     self,
            ovr:    unsafe { zeroed() }
        }, buf).map(|_| ()))
    }
}

struct ServerAsyncRead<'a, T: 'a> { 
    np:     &'a NamedPipeServer<T>,
    ovr:    OVERLAPPED,
}
impl<'a, T> Read for ServerAsyncRead<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut len = 0;
        if unsafe { ReadFile(self.np.handle as HANDLE, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut len, &mut self.ovr) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(len as usize)
    }
}
impl<'a, T> AsyncRead for ServerAsyncRead<'a, T> { }
impl<'a, T> Future for ServerAsyncRead<'a, T> 
    where T: de::DeserializeOwned
{
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut buf = vec![0u8; 65536];
        match self.read_buf(&mut buf)? {
            Async::Ready(len) => {
                return Ok(Async::Ready(Deserialize::deserialize(&mut Deserializer::new(&buf[buf.len() - len as usize..])).unwrap()));
            },
            Async::NotReady => return Ok(Async::NotReady)
        }
    }
}

impl<T> Receiver for NamedPipeServer<T> 
    where T: de::DeserializeOwned
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a> {
        use std::mem::zeroed;
        Box::new(ServerAsyncRead {
            np:     self,
            ovr:    unsafe { zeroed() },
        })
    }
} 

pub struct NamedPipeServer<T> {
    handle:     usize,
    ovr:        Cell<Option<[u8; mem::size_of::<OVERLAPPED>()]>>,
    _phantom:   PhantomData<T>,
}
impl<T> NamedPipeServer<T> 
    where T: Serialize
{
    pub fn new<S: AsRef<OsStr>>(name: S) -> Result<NamedPipeServer<T>> {
        let mut n = OsString::from("\\\\.\\pipe\\".to_owned());
        n.push(name);
        n.push("\x00");
        let n = n.as_os_str().encode_wide().collect::<Vec<u16>>();
        let h = unsafe { 
            CreateNamedPipeW(n.as_ptr(), 0x40000000 | 0x3, 0x4 | 0x2, 255, 65536, 65536, 0, null_mut()) 
        };
        if h == -1isize as HANDLE {
            return Err(Error::last_os_error());
        }
        let mut ovr : OVERLAPPED = unsafe { mem::zeroed() };
        if unsafe { ConnectNamedPipe(h, &mut ovr) } == 0 {
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(997) {
                return Err(e);
            }
        }
        Ok(NamedPipeServer {
            handle:     h as usize,
            ovr:        Cell::new(Some(unsafe { mem::transmute(ovr) })),
            _phantom:   PhantomData
        })
    }
} 
impl<T> Drop for NamedPipeServer<T> {
    fn drop(&mut self) {
        unsafe { DisconnectNamedPipe(self.handle as HANDLE); }
        unsafe { CloseHandle(self.handle as HANDLE); }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NamedPipeClient<T> {
    np_name:    Vec<u16>,
    handle:     usize,
    _phantom:   PhantomData<T>
}
impl<T> NamedPipeClient<T> 
    where T: de::DeserializeOwned
{
    /// Blocks until it connects to the server
    pub fn new<S: AsRef<OsStr>>(name: S) -> Result<NamedPipeClient<T>> {
        use winapi::um::winnt::{ FILE_WRITE_ATTRIBUTES, GENERIC_READ, GENERIC_WRITE };
        let mut n = OsString::from("\\\\.\\pipe\\".to_owned());
        n.push(name);
        n.push("\x00");
        let n = n.as_os_str().encode_wide().collect::<Vec<u16>>();
        if unsafe {  WaitNamedPipeW(n.as_ptr(), 0xffffffff) } == 0 {
            return Err(Error::last_os_error());
        }
        let handle = unsafe {
            CreateFileW(n.as_ptr(), GENERIC_READ | FILE_WRITE_ATTRIBUTES | GENERIC_WRITE, 1, null_mut(), 3, 0, null_mut())
        };
        if handle == -1isize as HANDLE {
            return Err(Error::last_os_error());
        }
        if unsafe { SetNamedPipeHandleState(handle, &mut 2, null_mut(), null_mut()) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(NamedPipeClient {
            np_name:    n,
            handle:     handle as usize,
            _phantom:   PhantomData
        })
    }
    pub fn recv(&self) -> Result<T> {
        let mut buf = vec![0u8; 65536];
        let mut len = 0;
        if unsafe { ReadFile(self.handle as HANDLE, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut len, null_mut()) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Deserialize::deserialize(&mut Deserializer::new(&buf[..len as usize])).unwrap())
    }
}
impl<T> Drop for NamedPipeClient<T> {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle as HANDLE); }
    }
}
impl<T: de::DeserializeOwned> Clone for NamedPipeClient<T> {
    fn clone(&self) -> NamedPipeClient<T> {
        NamedPipeClient::new(String::from_utf16(&self.np_name).unwrap()).unwrap()
    }
}

struct ClientAsyncWrite<'a, T: 'a> {
    np:     &'a NamedPipeClient<T>,
    ovr:    OVERLAPPED
} 

impl<'a, T> AsyncWrite for ClientAsyncWrite<'a, T> {
    fn shutdown(&mut self) -> Poll<(), Error> {
        if unsafe { DisconnectNamedPipe(self.np.handle as HANDLE) } == 0 {
            return Err(Error::last_os_error());
        }
        if unsafe { CloseHandle(self.np.handle as HANDLE) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(Async::Ready(()))
    }
    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, Error> {
        if !buf.has_remaining() {
            return Ok(Async::Ready(0));
        }
        if self.ovr.hEvent as usize == 0 {
            match self.write(buf.bytes()) {
                Err(e) => {
                    if e.raw_os_error() != Some(997) {
                        return Err(e);
                    }
                },
                Ok(x) => {
                    return Ok(Async::Ready(x));
                }
            }
        } 
        // We have already told the OS to write
        let mut n_bytes = 0;
        if unsafe { GetOverlappedResult(self.np.handle as HANDLE, &mut self.ovr, &mut n_bytes, 0) } == 0 {
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(996) {
                return Err(e);
            }
            return Ok(Async::NotReady);
        }
        Ok(Async::Ready(n_bytes as usize))
    }
}
impl<'a, T> Write for ClientAsyncWrite<'a, T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let handle = self.np.handle;
        let mut n_bytes = 0;
        if unsafe{ WriteFile(handle as HANDLE, buf.as_ptr() as *const c_void, buf.len() as u32, &mut n_bytes, &mut self.ovr) } == 0 {
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(997) {
                return Err(e);
            }
        }
        Ok(n_bytes as usize)
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

struct ClientAsyncRead<'a, T: 'a> { 
    np:     &'a NamedPipeClient<T>,
    ovr:    OVERLAPPED,
}
impl<'a, T> Read for ClientAsyncRead<'a, T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut len = 0;
        if unsafe { ReadFile(self.np.handle as HANDLE, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut len, &mut self.ovr) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(len as usize)
    }
}
impl<'a, T> AsyncRead for ClientAsyncRead<'a, T> { }
impl<'a, T> Future for ClientAsyncRead<'a, T> 
    where T: de::DeserializeOwned
{
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut buf = vec![0u8; 65536];
        match self.read_buf(&mut buf)? {
            Async::Ready(len) => {
                return Ok(Async::Ready(Deserialize::deserialize(&mut Deserializer::new(&buf[..len as usize])).unwrap()));
            },
            Async::NotReady => return Ok(Async::NotReady)
        }
    }
}

impl<T> Receiver for NamedPipeClient<T> 
    where T: de::DeserializeOwned
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_recv<'a>(&'a self) -> Box<Future<Item = Self::Item, Error = Error> + 'a> {
        use std::mem::zeroed;
        Box::new(ClientAsyncRead {
            np:     self,
            ovr:    unsafe { zeroed() },
        })
    }
} 

impl<T> Sender for NamedPipeClient<T> 
    where T: serde::Serialize 
{
    type Item = T;

    /// This function will block until there is a client connected
    fn async_send<'a>(&'a self, to_send: Self::Item) -> Box<Future<Item = (), Error = Error> + 'a> { 
        use tokio_io::io::write_all;
        use std::mem::zeroed;
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        Box::new(write_all(ClientAsyncWrite {
            np:     self,
            ovr:    unsafe { zeroed() }
        }, buf).map(|_| ()))
    }
} 
