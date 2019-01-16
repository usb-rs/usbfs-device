//! A wrapper for the Linux [USB Filesystem](https://kernel.readthedocs.io/en/sphinx-samples/usb.html#the-usb-filesystem-usbfs).
#![deny(rust_2018_idioms, future_incompatible, missing_docs)]

use std::ffi;
use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::io::FromRawFd;
use std::os::unix::io::IntoRawFd;
use std::os::unix::io::RawFd;
use std::path::Path;
//use std::cell::RefCell;
//use std::collections::HashMap;
use byteorder::{LittleEndian, WriteBytesExt};

// TODO: derive using bindgen?
const USB_DIR_OUT: u8 = 0x00;
const USB_DIR_IN: u8 = 0x80;

// TODO: if URBs aren't reap()'d, their memory leaks

/*
struct Completion {
    callback: Box<dyn Fn(Result<UrbWrap, nix::Error>)>,
}

struct CompletionSet {
    completion_seq: usize,
    // TODO: other containers?  e.g. slab?
    completions: HashMap<usize, Completion>
}
impl Default for CompletionSet {
    fn default() -> Self {
        CompletionSet {
            completion_seq: 0,
            completions: HashMap::new(),
        }
    }
}
*/

struct DeviceInner {
    fd: RawFd,
}
// https://users.rust-lang.org/t/doing-asynnchronous-serial/8412
impl mio::Evented for DeviceInner {
    fn register(
        &self,
        poll: &mio::Poll,
        token: mio::Token,
        interest: mio::Ready,
        opts: mio::PollOpt,
    ) -> io::Result<()> {
        mio::unix::EventedFd(&self.fd).register(poll, token, interest, opts)
    }

    fn reregister(
        &self,
        poll: &mio::Poll,
        token: mio::Token,
        interest: mio::Ready,
        opts: mio::PollOpt,
    ) -> io::Result<()> {
        mio::unix::EventedFd(&self.fd).reregister(poll, token, interest, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> io::Result<()> {
        mio::unix::EventedFd(&self.fd).deregister(poll)
    }
}

/// User-space control over a particular USB device.
pub struct DeviceHandle {
    io: tokio::reactor::PollEvented2<DeviceInner>,
    //completions: RefCell<CompletionSet>
}
impl DeviceHandle {
    /// Creates a new `DeviceHandle` instances from a filesystem path.
    ///
    /// The user of the current process must have read and write access to the given file.
    ///
    /// e.g. `DeviceHandle::new_from_path("/dev/bus/usb/001/022")`
    pub fn new_from_path(path: &Path) -> io::Result<DeviceHandle> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|f| DeviceHandle::new(f.into_raw_fd()))
    }
    fn new(fd: RawFd) -> DeviceHandle {
        let inner = DeviceInner { fd };
        DeviceHandle {
            io: tokio::reactor::PollEvented2::new(inner),
            //completions: RefCell::new(CompletionSet::default()),
        }
    }
    /*
        fn add_completion<F>(&self, f: F)
        where
            F: FnOnce(usize)->Option<Completion>
        {
            let mut completions = self.completions.borrow_mut();
            completions.completion_seq += 1;
            let seq = completions.completion_seq;
            if let Some(completion) = f(seq) {
                completions.completions.insert(seq, completion);
            }
        }
    */
    fn capabilities(&self) -> Result<Capabilities, ()> {
        let mut caps: usbfs_sys::types::__u32 = 0;
        match unsafe { usbfs_sys::ioctl::get_capabilities(self.fd(), &mut caps) } {
            Err(e) => panic!("TODO {:?}", e),
            Ok(_) => Ok(Capabilities::from_bits_truncate(caps)),
        }
    }

    fn read_descriptors(&self) -> Result<(), io::Error> {
        let mut f = unsafe { fs::File::from_raw_fd(self.fd()) };
        let mut buf = vec![];
        f.read_to_end(&mut buf)?;
        // seek back to the start of the file, in case another read is performed later
        f.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    /// Obtain a reference to the given interface of the USB device, in an 'unclaimed' state.
    pub fn interface(&self, iface: u32) -> UnclaimedInterface<'_> {
        UnclaimedInterface { dev: self, iface }
    }

    // take `&mut self` so that no objects with borrowed references to self may exist when this is
    // called (because a reset would potentially invalidate those objects in any case).
    /// Ask the kernel to perform a USB bus reset on this device
    pub fn reset(&mut self) -> Result<(), io::Error> {
        match unsafe { usbfs_sys::ioctl::reset(self.fd()) } {
            Err(e) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("reset: {:?}", e),
            )),
            Ok(_) => Ok(()),
        }
    }

    unsafe fn mmap(&self, len: usize) -> nix::Result<*mut std::ffi::c_void> {
        nix::sys::mman::mmap(
            std::ptr::null_mut(),
            len,
            nix::sys::mman::ProtFlags::PROT_READ | nix::sys::mman::ProtFlags::PROT_READ,
            nix::sys::mman::MapFlags::MAP_SHARED,
            self.fd(),
            0,
        )
    }

    /// Used to 'harvest' a URB previously submitted to an endpoint pipe
    pub fn reap_ndelay(&self) -> nix::Result<UrbWrap> {
        let mut urb_p: *mut std::ffi::c_void = std::ptr::null_mut();
        match unsafe { usbfs_sys::ioctl::reapurbndelay(self.fd(), &mut urb_p) } {
            Err(e) => Err(e),
            Ok(_) => Ok(UrbWrap(unsafe {
                Box::from_raw(urb_p as *mut usbfs_sys::types::urb)
            })),
        }
    }
    /*
        /// Used to 'harvest' a URB previously submitted to an endpoint pipe
        pub fn reap_cb(&self) -> nix::Result<()> {
            let mut urb_p: *mut std::ffi::c_void = std::ptr::null_mut();
            loop {
                let urb = self.reap_ndelay()?;
                self.dispatch(urb)?;
            }
        }
    */
    fn poll_reap(&self) -> futures::Poll<UrbWrap, io::Error> {
        // oddly, we need to poll for write-readiness to determine if we can reap a URB response
        futures::try_ready!(self.io.poll_write_ready());
        // TODO: loop until EAGAIN?
        match self.reap_ndelay() {
            Ok(ret) => Ok(futures::Async::Ready(ret)),
            Err(nix::Error::Sys(nix::errno::Errno::EAGAIN)) => {
                self.io.clear_write_ready()?;
                Ok(futures::Async::NotReady)
            }
            Err(nix::Error::Sys(e)) => Err(io::Error::from(e)),
            Err(e) => Err(io::Error::new(io::ErrorKind::Other, format!("{:?}", e))),
        }
    }
    /// Returns a Future that will resolve to the next completed URB
    pub fn reap(self) -> Reap {
        Reap { dev: Some(self) }
    }
    /*
        fn dispatch(&self, urb: UrbWrap) -> nix::Result<()> {
            let mut completions = self.completions.borrow_mut();
            let id = urb.id();
            let result = match urb.0.status {
                0 => Ok(urb),
                e => Err(nix::Error::Sys(nix::errno::Errno::from_i32(e)))
            };
            match completions.completions.remove(&id) {
                Some(c) => (c.callback)(result),
                None => panic!("No completion for id {}", id),
            }
            Ok(())
        }
    */
    fn fd(&self) -> RawFd {
        self.io.get_ref().fd
    }
}

/// A future which will resolve to a completed USB Request Block (URB).
pub struct Reap {
    dev: Option<DeviceHandle>,
}
impl futures::Future for Reap {
    type Item = (DeviceHandle, UrbWrap);
    type Error = io::Error;

    fn poll(&mut self) -> Result<futures::Async<Self::Item>, Self::Error> {
        let urb = {
            let ref mut inner = self
                .dev
                .as_mut()
                .expect("RecvDgram polled after completion");

            futures::try_ready!(inner.poll_reap())
        };

        let inner = self.dev.take().unwrap();
        Ok(futures::Async::Ready((inner, urb)))
    }
}

/// Should we request that any kernel driver reconnect on release of our claim on an interface,
/// or leave the kernel driver disconnected when our own claim is released?
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ReconnectOptions {
    /// Specifies that we should ask the kernel to reconnect any kernel driver when out own claim
    /// is released
    ReconnectOnRelease,
    /// Specifies that we shouldn't to anything more when our claim is released, so if a kernel
    /// driver was connected before we claimed the interface, it will remain disconnected
    /// afterwards
    StayDisconnectedOnRelease,
}

/// Describes how to handle any driver which may be already connected to a USB interface that
/// we want to claim for ourselves
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DisconnectOptions<'a> {
    /// Disconnect the existing driver if it's name is the given value
    DisconnectIf(&'a str, ReconnectOptions),
    /// Disconnect the existing driver except if it's name is the given value
    DisconnectExcept(&'a str, ReconnectOptions),
    /// Leave any existing driver connected
    LeaveConnected,
}

/// The name of the driver in use for a particular interface of the USB device.
/// if the driver is `Driver::UsbFs`, then the interface is already claimed by _usbdeffs_ (whether
/// by the current process, or some other).
#[derive(Clone, PartialEq, Debug)]
pub enum Driver {
    /// indicates that the driver name is `"usbfs"`
    UsbFs,
    /// the driver name (which is something other than `"usbfs"`)
    Other(String),
}

/// A handle on a specific interface of a given `DeviceHandle`.  An `UnclaimedInterface` allows
/// the name of the current driver connected to the interface within the kernel to be queried,
/// and may be converted to a `ClaimedInterface` as long as no other driver is connected.
pub struct UnclaimedInterface<'dev> {
    dev: &'dev DeviceHandle,
    iface: u32,
}
impl<'dev> UnclaimedInterface<'dev> {
    /// returns the name of the driver currently attached to the interface.
    pub fn driver(&self) -> Result<Driver, io::Error> {
        let mut driver = usbfs_sys::types::getdriver {
            interface: self.iface,
            driver: [0; usbfs_sys::types::MAXDRIVERNAME as usize + 1],
        };
        match unsafe { usbfs_sys::ioctl::getdriver(self.dev.fd(), &mut driver) } {
            Err(e) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("getdriver: {:?}", e),
            )),
            Ok(_) => {
                // TODO: safety in face of the buffer failing to contain a nul byte?
                let driver_name = unsafe { ffi::CStr::from_ptr(driver.driver.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                Ok(if driver_name == "usbfs" {
                    Driver::UsbFs
                } else {
                    Driver::Other(driver_name)
                })
            }
        }
    }

    /// Attempt to claim this interface for use via the current device filehandle.
    ///
    /// The given `DisconnectOptions` specify how any driver already connected should be handled.
    /// If a driver is connected when we try to claim the interface, the claim will fail with
    /// `EBUSY`.
    pub fn disconnect_claim(
        self,
        disconnect: DisconnectOptions<'_>,
    ) -> Result<ClaimedInterface<'dev>, io::Error> {
        let mut request = usbfs_sys::types::disconnect_claim {
            interface: self.iface,
            flags: 0,
            driver: [0; usbfs_sys::types::MAXDRIVERNAME as usize + 1],
        };
        let mut reconnect = ReconnectOptions::StayDisconnectedOnRelease;
        match disconnect {
            DisconnectOptions::LeaveConnected => (),
            DisconnectOptions::DisconnectExcept(driver, recon) => {
                request.flags = usbfs_sys::types::DISCONNECT_CLAIM_EXCEPT_DRIVER;
                for (n, b) in driver.as_bytes().iter().enumerate() {
                    request.driver[n] = *b as i8;
                }
                reconnect = recon;
            }
            DisconnectOptions::DisconnectIf(driver, recon) => {
                request.flags = usbfs_sys::types::DISCONNECT_CLAIM_IF_DRIVER;
                for (n, b) in driver.as_bytes().iter().enumerate() {
                    request.driver[n] = *b as i8;
                }
                reconnect = recon;
            }
        }
        match unsafe { usbfs_sys::ioctl::disconnect_claim(self.dev.fd(), &mut request) } {
            Err(e) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("disconnect_claim: {:?}", e),
            )),
            Ok(_) => Ok(ClaimedInterface {
                dev: self.dev,
                iface: self.iface,
                reconnect,
            }),
        }
    }
}

/// Largest allowed endpoint number
pub const ENDPOINT_MAX: u8 = 0x0f;

/// An interface which has been claimed for use via this _usbdevfs_ filehandle
pub struct ClaimedInterface<'dev> {
    dev: &'dev DeviceHandle,
    iface: u32,
    reconnect: ReconnectOptions,
}
impl<'dev> Drop for ClaimedInterface<'dev> {
    fn drop(&mut self) {
        /*        let mut iface = self.iface;
                match unsafe { usbfs_sys::ioctl::releaseinterface(self.dev.fd(), &mut iface) } {
                    Err(e) => eprintln!("ClaimedInterface::drop() failed: {:?}", e),
                    Ok(_) => (),
                }
                if self.reconnect == ReconnectOptions::ReconnectOnRelease {
                    match unsafe { usbfs_sys::ioctl::connect(self.dev.fd(), iface as i32) } {
                        Err(e) => eprintln!("ClaimedInterface::drop() failed: {:?}", e),
                        Ok(_) => (),
                    }

                }
        */
    }
}
impl<'dev> ClaimedInterface<'dev> {
    /// Create an endpoint object to send and receive data from the given endpoint of the interface
    ///
    /// Panics if `endpoint` is greater than `ENDPOINT_MAX`.
    pub fn endpoint_control(&self, endpoint: u8) -> ControlPipe<'_> {
        assert!(endpoint <= ENDPOINT_MAX);
        ControlPipe {
            dev: self.dev,
            iface: self.iface,
            endpoint,
        }
    }
}

/// The different types of endpoint a device may support
pub enum EndpointType {
    /// Bulk transfer endpoint that makes no guarantees of throughput or latency, but which can use
    /// any free USB bandwith not used for other transfers.
    Bulk,
    /// Control endpoint for small control requests and responses.
    Control,
    /// Interrupt transfers with bounded latency but low throughput.
    Interrupt,
    /// Transfer type reserving a portion of USB bandwidth in order to provide throughbut and
    /// latency guarentees (e.g. realtime streaming).
    Isochronous,
}

/// The direction in which the request seeks to send data
pub enum TransferDirection {
    /// A request sending data to the USB device
    HostToDevice,
    /// A request to recieve data from the USB device
    DeviceToHost,
}

/// The kind of request being made
pub enum ReqType {
    /// A request defined in the USB standard
    Standard,
    /// A request defined for a particular USB device class
    Class,
    /// Vendor-defined / device-specific request type
    Vendor,
    /// Not used
    Reserved,
}

/// The element of the USB device to which the request is directed
pub enum Recipient {
    /// The request is for the device
    Device,
    /// The request is for a specific interface of the device
    Interface,
    /// The request is for a specific endpoint of an interface
    Endpoint,
    /// Destination other than `Device`, `Interface` or `Endpoint`
    Other,
    /// Not used
    Reserved(u8),
}

/// A USB control request.
///
/// Includes the fields of the USB _SETUP_ packet, and a `data` buffer for contents of _DATA_
/// packets.
pub struct ControlRequest {
    /// The _Data Phase Transfer Direction_ component of the USB `bmRequestType` field
    pub direction: TransferDirection,
    /// The _Type_ component of the USB `bmRequestType` field
    pub req_type: ReqType,
    /// The _Recipient_ component of the USB `bmRequestType` field
    pub recipient: Recipient,
    /// `bRequest`
    pub request: u8,
    /// `wValue`
    pub value: u16,
    /// `wIndex`
    pub index: u16,
    /// length of data provides the value for the USB _SETUP_ packet `wLength` field
    pub data: Vec<u8>,
}
impl ControlRequest {
    fn usb_request_type(&self) -> u8 {
        (match self.direction {
            TransferDirection::HostToDevice => 0b0000_0000,
            TransferDirection::DeviceToHost => 0b1000_0000,
        }) + (match self.req_type {
            ReqType::Standard => 0b0000_0000,
            ReqType::Class => 0b0010_0000,
            ReqType::Vendor => 0b0100_0000,
            ReqType::Reserved => 0b0110_0000,
        }) + match self.recipient {
            Recipient::Device => 0b0000_0000,
            Recipient::Interface => 0b0000_0001,
            Recipient::Endpoint => 0b0000_0010,
            Recipient::Other => 0b0000_0011,
            Recipient::Reserved(v) => v,
        }
    }
}

/// Commuinication channel to an endpoint on an interface of a USB device
pub struct ControlPipe<'dev> {
    dev: &'dev DeviceHandle,
    iface: u32,
    endpoint: u8,
}
impl<'dev> ControlPipe<'dev> {
    const SETUP_LEN: usize = 8;

    /// Send a control request asynchronously to this endpoint.
    ///
    /// TODO: In order to see the result, ...
    ///
    /// Implemented in terms of the `USBDEVFS_SUBMITURB` ioctl.
    pub fn submit(
        &self,
        req: ControlRequest, /*, callback: CB*/
    ) -> Result<ResponseFuture, io::Error>
//where CB: Fn(Result<UrbWrap,nix::Error>) + 'static
    {
        if req.data.len() > std::i16::MAX as usize - Self::SETUP_LEN {
            return Err(io::Error::new(io::ErrorKind::Other, "input too large"));
        }

        let mut data = Vec::with_capacity(req.data.len() + Self::SETUP_LEN);
        data.write_u8(req.usb_request_type()).unwrap();
        data.write_u8(req.request).unwrap();
        data.write_u16::<LittleEndian>(req.value).unwrap();
        data.write_u16::<LittleEndian>(req.index).unwrap();
        data.write_u16::<LittleEndian>(req.data.len() as u16)
            .unwrap();
        // TODO: pointless to copy the arg if this is a read,
        data.extend_from_slice(&req.data[..]);
        let buffer_length = data.len() as i32;
        let mut data = data;
        let id = 1; // TODO
        let request = Box::new(usbfs_sys::types::urb {
            type_: usbfs_sys::types::URB_TYPE_CONTROL,
            endpoint: self.endpoint | USB_DIR_OUT,
            status: 0,
            flags: 0,
            buffer: data.as_mut_ptr() as *mut std::ffi::c_void,
            buffer_length,
            actual_length: 0,
            start_frame: 0,
            __bindgen_anon_1: usbfs_sys::types::urb__bindgen_ty_1 {
                number_of_packets: 0,
            },
            error_count: 0,
            signr: 0,
            usercontext: id as *mut std::ffi::c_void,
            iso_frame_desc: usbfs_sys::types::__IncompleteArrayField::new(),
        });

        std::mem::forget(data); // 😦 hopefully handled by impl Drop for UrbWrap

        match unsafe { usbfs_sys::ioctl::submiturb(self.dev.fd(), Box::into_raw(request)) } {
            Err(e) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("submiturb: {:?}", e),
            )),
            Ok(_) => Ok(ResponseFuture {}),
        }
    }
}

/// TODO!
pub struct ResponseFuture {}

/// Wrapper around a URB result value
///
/// TODO: rework this into something useful
pub struct UrbWrap(Box<usbfs_sys::types::urb>);
impl fmt::Debug for UrbWrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("Urb")
            .field("type", &self.0.type_)
            .field("endpoint", &self.0.endpoint)
            .field("status", &self.0.status)
            .field("flags", &self.0.flags)
            .field("error_count", &self.0.error_count)
            .field("signr", &self.0.signr)
            .field("usercontext", &self.0.usercontext)
            .field("buffer_length", &self.0.buffer_length)
            .finish()
    }
}
impl UrbWrap {
    /// The `usercontext` id assigned to this URB when it was submitted.
    pub fn id(&self) -> usize {
        self.0.usercontext as usize
    }
    /// The data bufer to which the URB refers.
    pub fn data(&self) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(self.0.buffer as *const u8, self.0.actual_length as usize)
        }
    }
}
impl Drop for UrbWrap {
    fn drop(&mut self) {
        let buf = unsafe {
            Vec::from_raw_parts(
                self.0.buffer as *mut u8,
                self.0.buffer_length as usize,
                self.0.buffer_length as usize,
            )
        };
        drop(buf);
    }
}
bitflags::bitflags! {
    struct Capabilities: u32 {
        const ZeroPacket = usbfs_sys::types::CAP_ZERO_PACKET;
        const BulkContinuation = usbfs_sys::types::CAP_BULK_CONTINUATION;
        const NoPacketSizeLim = usbfs_sys::types::CAP_NO_PACKET_SIZE_LIM;
        const BulkScatterGather = usbfs_sys::types::CAP_BULK_SCATTER_GATHER;
        const CapReapAfterDisconnect = usbfs_sys::types::CAP_REAP_AFTER_DISCONNECT;
        const MMap = usbfs_sys::types::CAP_MMAP;
        const DropPrivileges = usbfs_sys::types::CAP_DROP_PRIVILEGES;
    }
}

#[cfg(test)]
mod test {
    use crate::*;
    use futures::future::Future;
    use std::path::Path;

    const DEV_PATH: &str = "/dev/bus/usb/001/011";

    #[test]
    fn reset() {
        let mut d = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        d.reset().unwrap();
    }

    #[test]
    fn getdriver() {
        let d = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        for i in 0..=2 {
            let iface = d.interface(i);
            println!("interface {} driver: {:?}", i, iface.driver());
        }
    }

    #[test]
    fn caps() {
        let d = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        println!("caps {:?}", d.capabilities().unwrap());
    }

    #[test]
    fn interface() {
        let d = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        let iface = d.interface(0);
        println!("iface 0 driver: {:?}", iface.driver());
        iface
            .disconnect_claim(DisconnectOptions::DisconnectExcept(
                "usbfs",
                ReconnectOptions::ReconnectOnRelease,
            ))
            .unwrap();
    }

    #[test]
    fn multiple_claims() {
        let d = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        let iface = d.interface(0);
        println!("iface 0 driver: {:?}", iface.driver());
        iface
            .disconnect_claim(DisconnectOptions::DisconnectExcept(
                "usbfs",
                ReconnectOptions::ReconnectOnRelease,
            ))
            .unwrap();

        let d_again = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        let iface_again = d_again.interface(0);
        assert!(iface_again
            .disconnect_claim(DisconnectOptions::DisconnectExcept(
                "usbfs",
                ReconnectOptions::ReconnectOnRelease
            ))
            .is_err());
    }

    #[test]
    fn future() {
        let mut runtime = tokio::runtime::Runtime::new().unwrap();

        let dev = DeviceHandle::new_from_path(Path::new(DEV_PATH)).unwrap();
        {
            let iface = dev.interface(0);
            let claimed = iface
                .disconnect_claim(DisconnectOptions::DisconnectIf(
                    "dvb_usb_rtl28xxu",
                    ReconnectOptions::StayDisconnectedOnRelease,
                ))
                .unwrap();
            let ctl_pipe = claimed.endpoint_control(0);
            let req = ControlRequest {
                direction: TransferDirection::HostToDevice,
                req_type: ReqType::Vendor,
                recipient: Recipient::Interface,
                request: 0x00,
                value: 0x3001,
                index: 0x0210,
                data: vec![0x09],
            };
            ctl_pipe.submit(req).unwrap();
        }

        let future = dev
            .reap()
            .map(|(dev, urb)| {
                println!("gotz URB {:?}", urb);
            })
            .map_err(|e| {
                eprintln!("failed: {:?}", e);
            });
        runtime.spawn(future);
        runtime.shutdown_on_idle().wait().unwrap();
    }
}
