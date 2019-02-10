# usbfs-device

A Rust API for accessing the
[Linux _usbfs_ API](https://kernel.readthedocs.io/en/sphinx-samples/usb.html#the-usb-filesystem-usbfs),
based on Rust's [Futures, version 0.1](https://docs.rs/futures/0.1).

⛔ **NB** so far only supports control messages; probably not useful for real USB devices yet.

This API provides,

 - A safe wrapper around underlying `ioctl()` operations of Linux _usbfs_
 - Means to take control of some interface of a device (detaching any Kernel driver if needed)
 - The ability to exchange data with a given endpoint of an interface
 
These are **not features** of the API,

 - No cross platform support -- use
   [libusb](https://crates.io/crates/libusb) if you want Windows and MacOS support too.
 - No support for parsing USB _descriptors_
 - No support for discovering the devices attached to the system (i.e. there's no API wrapper to find the relevant
   entries in Linux _sysfs_ etc.)
   
(Maybe other crates will pick up some of those.)

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
   - [x] Clear halt-condition (seems this has to be blocking though?)
   - [x] Control transfers
   - [ ] Isochronous transfers
   - [ ] Bulk transfers
   - [ ] Interrupt transfers
