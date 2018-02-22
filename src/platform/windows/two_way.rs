extern crate winapi;
extern crate serde;
extern crate rmp_serde;
use winapi::um::winsock2::*;
use winapi::shared::ws2def::*; 
use std::marker::PhantomData;
use serde::{ Serialize, de, Deserialize };
use rmp_serde::{ Serializer, Deserializer };
use std::io::{ Result, Error };
use std::cell::Cell; 
use std::ptr::null_mut;

#[repr(C)]
struct sockaddr_un {
    pub sun_family:     u16, 
    pub sun_path:       [u8; 108]
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnixSockStream<T> { 
    connect_socket: SOCKET,
    socket:         Cell<SOCKET>,
    _phantom:       PhantomData<T>
}
impl<T> UnixSockStream<T> 
    where T: de::DeserializeOwned + Serialize
{
    pub fn new<S: AsRef<str>>(name: S) -> Result<UnixSockStream<T>> { 
        use std::ptr::copy_nonoverlapping;
        use winapi::um::winsock2::SOCK_STREAM;
        let name : &str = name.as_ref();
        assert!(name.len() < 107);
        let mut sock = sockaddr_un { 
            sun_family:     AF_UNIX as u16, 
            sun_path:       [0; 108]
        }; 
        unsafe { copy_nonoverlapping(name.as_ptr(), &mut sock.sun_path[1], name.len()) };
        let socket = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if socket == INVALID_SOCKET {
            return Err(Error::last_os_error());
        }
        let mut def = INVALID_SOCKET;
        if unsafe { bind(socket, &sock as *const sockaddr_un as *const SOCKADDR, 4 + name.len() as i32) } != 0 || unsafe { listen(socket, 1) } != 0 { 
            let e = Error::last_os_error();
            if e.raw_os_error() != Some(WSAEADDRINUSE) {
                return Err(Error::last_os_error());
            }
            def = socket;
            if unsafe { connect(socket, &sock as *const sockaddr_un as *const SOCKADDR, 4 + name.len() as i32) } != 0 {
                return Err(Error::last_os_error());
            }
        }
        Ok(UnixSockStream {
            connect_socket:     socket,
            socket:             Cell::new(def),
            _phantom:           PhantomData
        }) 
    }
    pub fn recv(&self) -> Result<T> {
        use std::mem;
        let mut socket = self.socket.get();
        if socket == INVALID_SOCKET { // Not connected
            socket = unsafe { accept(self.connect_socket, null_mut(), null_mut()) };
            if socket == INVALID_SOCKET {
                return Err(Error::last_os_error());
            }
            self.socket.set(socket);
        }
        let mut size = 0i32;
        if unsafe { recv(socket, &mut size as *mut i32 as *mut i8, mem::size_of::<i32>() as i32, 0) } != mem::size_of::<i32>() as i32 {
            return Err(Error::last_os_error());
        }
        let mut buf = vec![0u8; size as usize];
        if unsafe { recv(socket, buf.as_mut_ptr() as *mut i8, size, 0) } != size {
            return Err(Error::last_os_error());
        }
        Ok(Deserialize::deserialize(&mut Deserializer::new(&buf[..])).unwrap())
    }
    pub fn send(&self, to_send: T) -> Result<()> {
        use std::mem; 
        let mut socket = self.socket.get();
        if socket == INVALID_SOCKET { // Not connected
            socket = unsafe { accept(self.connect_socket, null_mut(), null_mut()) };
            if socket == INVALID_SOCKET {
                return Err(Error::last_os_error());
            }
            self.socket.set(socket);
        }
        let mut buf = Vec::new();
        to_send.serialize(&mut Serializer::new(&mut buf)).unwrap();
        if unsafe { send(socket, &(buf.len() as i32) as *const i32 as *const i8, mem::size_of::<usize>() as i32, 0) } == 0 {
            return Err(Error::last_os_error());
        }
        if unsafe { send(socket, buf.as_ptr() as *const i8, buf.len() as i32, 0) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}
