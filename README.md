# usbfs-device

A Rust API for accessing the
[Linux _usbfs_ API](https://kernel.readthedocs.io/en/sphinx-samples/usb.html#the-usb-filesystem-usbfs),
based on Rust's [Futures, version 0.1](https://docs.rs/futures/0.1).

This API provides,

 - A safe wrapper around the non-blocking `ioctl()` operations of usbfs
 - Means to take control of some interface of a device (detaching any Kernel driver if needed)
 - The ability to exchange data with a given endpoint of an interface
 
These are **not features** of the API,

 - No cross platform support -- this is not an abstraction for Linux / Windows / MacOS APIs -- use
   [libusb](https://crates.io/crates/libusb) for that instead.
 - No support for parsing USB 'descriptors'
 - No support for discovering the devices attached to the system (i.e. there's no API wrapper to find the relevant
   entries in Linux _sysfs_ etc.)
   
The above mentioned non-features could come to be supported by other crates, but I don't think there are any just yet.


## Supported _usbfs_ features

 - Device management
   - [x] Current kernel driver name retrieval
   - [x] Device reset
   - [ ] Capability query
   - [ ] Device configuration selection
   - [ ] Device connection info (speed)
 - Interface Management
   - [x] Interface claiming (with optional automatic release)
   - [ ] Interface 'alternate setting' selection
 - Endpoint management
   - [ ] Halt-condition clearing
   - [x] Control transfers
   - [ ] Isochronous transfers
   - [ ] Bulk transfers
   - [ ] Interrupt transfers
