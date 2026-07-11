//! IOKit-based USB transport for MTP on macOS.
//!
//! Replaces the rusb/libusb transport that fails on data-out operations.
//! Uses IOUSBDeviceInterface/IOUSBInterfaceInterface directly via FFI.

use super::iokit_ffi::*;
use crate::MtpError;
use std::ffi::CString;
use std::os::raw::c_void;

/// True when `kr` is an IOKit system error (top 16 bits `0xe000`) that's
/// worth attempting `ClearPipeStall` recovery against — pipe stalls
/// (`0xe0004xxx`, `sub_iokit_usb`) and transient conditions like
/// `kIOReturnNotResponding` / `kIOReturnAborted` (`0xe00002xx`,
/// `sub_iokit_common`) that commonly wedge the bulk pipes and are typically
/// clearable. Truly unrecoverable errors (device detached, permission denied)
/// still fail on retry and get reported with both kr codes in the message.
fn is_recoverable_iokit_error(kr: IOReturn) -> bool {
    (kr as u32) & 0xffff_0000 == 0xe000_0000
}

/// USB transport using macOS IOKit for bulk I/O.
pub struct IokitTransport {
    device: *mut *mut IOUSBDeviceInterface,
    interface: *mut *mut IOUSBInterfaceInterface,
    pipe_in: u8,
    pipe_out: u8,
    #[allow(dead_code)]
    max_packet_size: u16,
}

// SAFETY: IokitTransport holds raw IOKit pointers that are not inherently Send.
// However, the transport is only ever used from a single background worker thread
// at a time — it is moved (not shared) to that thread and all USB I/O is
// sequential. The Send impl allows that ownership transfer.
unsafe impl Send for IokitTransport {}

impl IokitTransport {
    /// Open a USB device by vendor/product ID and claim its MTP interface.
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, MtpError> {
        unsafe { Self::open_inner(vendor_id, product_id) }
    }

    unsafe fn open_inner(vendor_id: u16, product_id: u16) -> Result<Self, MtpError> {
        // Find the USB device via IOKit service matching.
        let class_name = CString::new(kUSBDeviceClassName).unwrap();
        let matching = IOServiceMatching(class_name.as_ptr());
        if matching.is_null() {
            return Err(MtpError::Usb("IOServiceMatching failed".to_string()));
        }

        // Add vendor/product filters to the matching dictionary.
        let vendor_key = cf_string("idVendor");
        let product_key = cf_string("idProduct");
        let vendor_num = CFNumberCreate(
            std::ptr::null(),
            kCFNumberSInt32Type,
            &(vendor_id as i32) as *const i32 as *const c_void,
        );
        let product_num = CFNumberCreate(
            std::ptr::null(),
            kCFNumberSInt32Type,
            &(product_id as i32) as *const i32 as *const c_void,
        );
        CFDictionarySetValue(
            matching,
            vendor_key as *const c_void,
            vendor_num as *const c_void,
        );
        CFDictionarySetValue(
            matching,
            product_key as *const c_void,
            product_num as *const c_void,
        );
        CFRelease(vendor_key as CFTypeRef);
        CFRelease(product_key as CFTypeRef);
        CFRelease(vendor_num as CFTypeRef);
        CFRelease(product_num as CFTypeRef);

        let mut iterator: io_iterator_t = 0;
        let kr = IOServiceGetMatchingServices(
            kIOMasterPortDefault,
            matching as CFDictionaryRef,
            &mut iterator,
        );
        if kr != kIOReturnSuccess {
            return Err(MtpError::Usb(format!(
                "IOServiceGetMatchingServices failed: {kr}"
            )));
        }

        let service = IOIteratorNext(iterator);
        IOObjectRelease(iterator);
        if service == 0 {
            return Err(MtpError::Usb(format!(
                "No USB device found with vid=0x{vendor_id:04x} pid=0x{product_id:04x}"
            )));
        }

        // Create plugin interface for the device.
        let plugin_type = cf_uuid(kIOUSBDeviceUserClientTypeID_str);
        let plugin_iface_id = cf_uuid(kIOCFPlugInInterfaceID_str);
        let mut plugin: *mut *mut IOCFPlugInInterface = std::ptr::null_mut();
        let mut score: SInt32 = 0;

        let kr = IOCreatePlugInInterfaceForService(
            service,
            plugin_type,
            plugin_iface_id,
            &mut plugin,
            &mut score,
        );
        IOObjectRelease(service);
        CFRelease(plugin_type as CFTypeRef);
        CFRelease(plugin_iface_id as CFTypeRef);

        if kr != kIOReturnSuccess || plugin.is_null() {
            return Err(MtpError::Usb(format!(
                "IOCreatePlugInInterfaceForService failed: {kr}"
            )));
        }

        // Query for the device interface.
        let device_iface_uuid = uuid_bytes(kIOUSBDeviceInterfaceID_str);
        let mut device: *mut *mut IOUSBDeviceInterface = std::ptr::null_mut();
        let hr = ((**plugin).QueryInterface)(
            plugin,
            device_iface_uuid,
            &mut device as *mut _ as *mut *mut c_void,
        );
        ((**plugin).Release)(plugin);

        if hr != 0 || device.is_null() {
            return Err(MtpError::Usb(format!(
                "QueryInterface for device failed: {hr}"
            )));
        }

        // Open the device.
        let kr = ((**device).USBDeviceOpen)(device);
        if kr != kIOReturnSuccess {
            ((**device).Release)(device);
            return Err(MtpError::Usb(format!("USBDeviceOpen failed: 0x{kr:08x}")));
        }

        // Wake up suspended device (aft does this in DeviceDescriptor constructor).
        // USBDeviceSuspend(dev, 0) resumes the device from suspend state.
        let _ = ((**device).USBDeviceSuspend)(device, 0);

        // Probe USB OS descriptors (aft does this; Zune may expect it).
        Self::probe_os_descriptors(device);

        // Set configuration (use config 1).
        let _ = ((**device).SetConfiguration)(device, 1);

        // Find and open the MTP interface.
        let request = IOUSBFindInterfaceRequest {
            bInterfaceClass: 6, // PTP/MTP Still Image class
            bInterfaceSubClass: 1,
            bInterfaceProtocol: 1,
            bAlternateSetting: 0xFFFF, // don't care
        };
        let mut iface_iterator: io_iterator_t = 0;
        let kr = ((**device).CreateInterfaceIterator)(device, &request, &mut iface_iterator);
        if kr != kIOReturnSuccess {
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(MtpError::Usb(format!(
                "CreateInterfaceIterator failed: 0x{kr:08x}"
            )));
        }

        let iface_service = IOIteratorNext(iface_iterator);
        IOObjectRelease(iface_iterator);
        if iface_service == 0 {
            // Try without class filter — some devices enumerate differently.
            let request_any = IOUSBFindInterfaceRequest {
                bInterfaceClass: 0xFFFF,
                bInterfaceSubClass: 0xFFFF,
                bInterfaceProtocol: 0xFFFF,
                bAlternateSetting: 0xFFFF,
            };
            let mut iface_iterator2: io_iterator_t = 0;
            let kr =
                ((**device).CreateInterfaceIterator)(device, &request_any, &mut iface_iterator2);
            if kr != kIOReturnSuccess {
                ((**device).USBDeviceClose)(device);
                ((**device).Release)(device);
                return Err(MtpError::Usb(
                    "No USB interfaces found on device".to_string(),
                ));
            }
            let iface_service2 = IOIteratorNext(iface_iterator2);
            IOObjectRelease(iface_iterator2);
            if iface_service2 == 0 {
                ((**device).USBDeviceClose)(device);
                ((**device).Release)(device);
                return Err(MtpError::Usb(
                    "No USB interfaces found on device".to_string(),
                ));
            }
            return Self::open_interface(device, iface_service2);
        }

        Self::open_interface(device, iface_service)
    }

    unsafe fn open_interface(
        device: *mut *mut IOUSBDeviceInterface,
        iface_service: io_service_t,
    ) -> Result<Self, MtpError> {
        // Create plugin for the interface (uses interface-specific type ID).
        let plugin_type = cf_uuid(kIOUSBInterfaceUserClientTypeID_str);
        let plugin_iface_id = cf_uuid(kIOCFPlugInInterfaceID_str);
        let mut plugin: *mut *mut IOCFPlugInInterface = std::ptr::null_mut();
        let mut score: SInt32 = 0;

        let kr = IOCreatePlugInInterfaceForService(
            iface_service,
            plugin_type,
            plugin_iface_id,
            &mut plugin,
            &mut score,
        );
        IOObjectRelease(iface_service);
        CFRelease(plugin_type as CFTypeRef);
        CFRelease(plugin_iface_id as CFTypeRef);

        if kr != kIOReturnSuccess || plugin.is_null() {
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(MtpError::Usb(format!(
                "IOCreatePlugInInterfaceForService (interface) failed: {kr}"
            )));
        }

        // Query for the interface interface.
        let iface_uuid = uuid_bytes(kIOUSBInterfaceInterfaceID_str);
        let mut interface: *mut *mut IOUSBInterfaceInterface = std::ptr::null_mut();
        let hr = ((**plugin).QueryInterface)(
            plugin,
            iface_uuid,
            &mut interface as *mut _ as *mut *mut c_void,
        );
        ((**plugin).Release)(plugin);

        if hr != 0 || interface.is_null() {
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(MtpError::Usb(format!(
                "QueryInterface for interface failed: {hr}"
            )));
        }

        // Open the interface.
        let kr = ((**interface).USBInterfaceOpen)(interface);
        if kr != kIOReturnSuccess {
            ((**interface).Release)(interface);
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(MtpError::Usb(format!(
                "USBInterfaceOpen failed: 0x{kr:08x}"
            )));
        }

        // Discover bulk endpoints.
        let mut num_endpoints: UInt8 = 0;
        ((**interface).GetNumEndpoints)(interface, &mut num_endpoints);

        let mut pipe_in: u8 = 0;
        let mut pipe_out: u8 = 0;
        let mut max_packet_size: u16 = 512;

        for pipe_ref in 1..=num_endpoints {
            let mut direction: UInt8 = 0;
            let mut number: UInt8 = 0;
            let mut transfer_type: UInt8 = 0;
            let mut mps: UInt16 = 0;
            let mut interval: UInt8 = 0;

            let kr = ((**interface).GetPipeProperties)(
                interface,
                pipe_ref,
                &mut direction,
                &mut number,
                &mut transfer_type,
                &mut mps,
                &mut interval,
            );
            if kr != kIOReturnSuccess {
                continue;
            }

            if transfer_type == kUSBBulk {
                if direction == kUSBIn && pipe_in == 0 {
                    pipe_in = pipe_ref;
                    max_packet_size = mps;
                } else if direction == kUSBOut && pipe_out == 0 {
                    pipe_out = pipe_ref;
                }
            }
        }

        if pipe_in == 0 || pipe_out == 0 {
            ((**interface).USBInterfaceClose)(interface);
            ((**interface).Release)(interface);
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(MtpError::Usb(
                "Could not find bulk IN/OUT endpoints".to_string(),
            ));
        }

        Ok(IokitTransport {
            device,
            interface,
            pipe_in,
            pipe_out,
            max_packet_size,
        })
    }

    /// Probe USB OS String Descriptor and Extended Compat ID,
    /// matching aft-mtp-cli's initialization sequence.
    unsafe fn probe_os_descriptors(device: *mut *mut IOUSBDeviceInterface) {
        // Read OS String Descriptor at index 0xEE.
        let mut req = IOUSBDevRequest {
            bmRequestType: 0x80, // Device-to-host, Standard, Device
            bRequest: 0x06,      // GET_DESCRIPTOR
            wValue: 0x03EE,      // String descriptor, index 0xEE
            wIndex: 0x0000,
            wLength: 64,
            pData: std::ptr::null_mut(),
            wLenDone: 0,
        };
        let mut buf = [0u8; 64];
        req.pData = buf.as_mut_ptr() as *mut c_void;
        let kr = ((**device).DeviceRequest)(device, &mut req);
        if kr == kIOReturnSuccess && req.wLenDone >= 18 {
            let vendor_code = buf[16];
            // Read Extended Compat ID using vendor code.
            let mut req2 = IOUSBDevRequest {
                bmRequestType: 0xC0, // Device-to-host, Vendor, Device
                bRequest: vendor_code,
                wValue: 0x0000,
                wIndex: 0x0004, // Extended Compat ID page
                wLength: 256,
                pData: std::ptr::null_mut(),
                wLenDone: 0,
            };
            let mut buf2 = [0u8; 256];
            req2.pData = buf2.as_mut_ptr() as *mut c_void;
            let _ = ((**device).DeviceRequest)(device, &mut req2);
        }
    }

    /// Write data to the bulk OUT endpoint.
    /// Writes in max-packet-size chunks, matching aft-mtp-cli behavior.
    pub fn write(&self, data: &[u8]) -> Result<usize, MtpError> {
        super::chunked_write(data, self.max_packet_size as usize, |chunk| {
            // SAFETY: We hold valid IOKit interface pointers.
            unsafe {
                let kr = ((**self.interface).WritePipe)(
                    self.interface,
                    self.pipe_out,
                    chunk.as_ptr() as *const c_void,
                    chunk.len() as UInt32,
                );
                if kr == kIOReturnSuccess {
                    return Ok(chunk.len());
                }
                if is_recoverable_iokit_error(kr) {
                    // Clear any host-side stall (prior ReadPipe timeouts, cable
                    // jostles, or Zune firmware hiccups can leave the OUT pipe
                    // wedged — every subsequent write then fails with the same
                    // error until the user physically replugs). Retry once.
                    let _ = ((**self.interface).ClearPipeStall)(self.interface, self.pipe_out);
                    let kr2 = ((**self.interface).WritePipe)(
                        self.interface,
                        self.pipe_out,
                        chunk.as_ptr() as *const c_void,
                        chunk.len() as UInt32,
                    );
                    if kr2 == kIOReturnSuccess {
                        return Ok(chunk.len());
                    }
                    return Err(MtpError::UsbFatal(format!(
                        "WritePipe failed: 0x{kr:08x} (retry after ClearPipeStall: 0x{kr2:08x})"
                    )));
                }
                Err(MtpError::Usb(format!("WritePipe failed: 0x{kr:08x}")))
            }
        })
    }

    /// Read data from the bulk IN endpoint.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, MtpError> {
        // SAFETY: We hold valid IOKit interface pointers.
        unsafe {
            let mut size = buf.len() as UInt32;
            let kr = ((**self.interface).ReadPipe)(
                self.interface,
                self.pipe_in,
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
            );
            if kr == kIOReturnSuccess {
                return Ok(size as usize);
            }
            if is_recoverable_iokit_error(kr) {
                let _ = ((**self.interface).ClearPipeStall)(self.interface, self.pipe_in);
                let mut size2 = buf.len() as UInt32;
                let kr2 = ((**self.interface).ReadPipe)(
                    self.interface,
                    self.pipe_in,
                    buf.as_mut_ptr() as *mut c_void,
                    &mut size2,
                );
                if kr2 == kIOReturnSuccess {
                    return Ok(size2 as usize);
                }
                return Err(MtpError::UsbFatal(format!(
                    "ReadPipe failed: 0x{kr:08x} (retry after ClearPipeStall: 0x{kr2:08x})"
                )));
            }
            Err(MtpError::Usb(format!("ReadPipe failed: 0x{kr:08x}")))
        }
    }

    /// Read with a timeout (in seconds). Uses a background thread since
    /// the base IOUSBInterfaceInterface100 doesn't have ReadPipeTO.
    pub fn read_with_timeout(&self, buf: &mut [u8], timeout_secs: u64) -> Result<usize, MtpError> {
        use std::sync::{Arc, Mutex};

        // SAFETY: The interface pointer remains valid for the duration of the read,
        // and we join the thread before returning. Cast to usize to cross Send boundary.
        let buf_len = buf.len();
        let shared_buf = Arc::new(Mutex::new(vec![0u8; buf_len]));
        let result = Arc::new(Mutex::new(None::<Result<usize, i32>>));

        let iface_addr = self.interface as usize;
        let pipe = self.pipe_in;
        let buf_clone = Arc::clone(&shared_buf);
        let result_clone = Arc::clone(&result);

        let handle = std::thread::spawn(move || {
            // SAFETY: The interface pointer (cast back from usize) is valid for
            // the duration of this thread because the caller either joins the
            // thread on success or calls AbortPipe to cancel the blocked ReadPipe
            // before joining. IOKit's AbortPipe is specifically designed to
            // safely cancel a ReadPipe that is blocked on another thread.
            let iface = iface_addr as *mut *mut IOUSBInterfaceInterface;
            let mut locked = buf_clone.lock().unwrap();
            let mut size = locked.len() as UInt32;
            let kr = unsafe {
                ((**iface).ReadPipe)(iface, pipe, locked.as_mut_ptr() as *mut c_void, &mut size)
            };
            let r = if kr != kIOReturnSuccess {
                Err(kr)
            } else {
                Ok(size as usize)
            };
            *result_clone.lock().unwrap() = Some(r);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            if std::time::Instant::now() > deadline {
                // Abort the pipe to unblock the read thread, then clear stalls
                // on BOTH bulk pipes. If we only abort the IN pipe, the OUT
                // pipe can remain out of sync with the device (it's still
                // waiting to ACK data we gave up on), and every subsequent
                // WritePipe call fails until the user physically replugs.
                unsafe {
                    ((**self.interface).AbortPipe)(self.interface, self.pipe_in);
                    ((**self.interface).ClearPipeStall)(self.interface, self.pipe_in);
                    ((**self.interface).ClearPipeStall)(self.interface, self.pipe_out);
                }
                let _ = handle.join();
                return Err(MtpError::UsbFatal(format!(
                    "ReadPipe timed out ({timeout_secs}s)"
                )));
            }
            if let Some(r) = result.lock().unwrap().take() {
                let _ = handle.join();
                match r {
                    Ok(n) => {
                        let locked = shared_buf.lock().unwrap();
                        buf[..n].copy_from_slice(&locked[..n]);
                        return Ok(n);
                    }
                    Err(kr) => {
                        return Err(MtpError::Usb(format!("ReadPipe failed: 0x{kr:08x}")));
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Read a full MTP container, reassembling multi-packet responses.
    pub fn read_container(&self) -> Result<Vec<u8>, MtpError> {
        self.read_container_with_timeout(30)
    }

    /// Like `read_container` but with a caller-supplied timeout in seconds.
    /// Used for operations that the Zune firmware handles slowly (e.g.
    /// `SetObjectPropValue` writing album art to flash can take up to ~60s
    /// under load; a 30s timeout there cascades into a wedged session).
    pub fn read_container_with_timeout(&self, timeout_secs: u64) -> Result<Vec<u8>, MtpError> {
        super::reassemble_container(|buf| self.read_with_timeout(buf, timeout_secs))
    }
}

impl Drop for IokitTransport {
    fn drop(&mut self) {
        // SAFETY: Releasing IOKit resources in reverse order.
        unsafe {
            ((**self.interface).USBInterfaceClose)(self.interface);
            ((**self.interface).Release)(self.interface);
            ((**self.device).USBDeviceClose)(self.device);
            ((**self.device).Release)(self.device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_recoverable_iokit_error_recognises_observed_codes() {
        // USB subsystem error observed mid-sync (WritePipe failures after the
        // OUT pipe stalled).
        assert!(is_recoverable_iokit_error(0xe000404fu32 as IOReturn));
        // Common IOKit error observed as the first failure (NotResponding).
        assert!(is_recoverable_iokit_error(0xe00002edu32 as IOReturn));
        // kIOUSBPipeStalled itself.
        assert!(is_recoverable_iokit_error(kIOUSBPipeStalled));
    }

    #[test]
    fn is_recoverable_iokit_error_rejects_non_iokit_codes() {
        assert!(!is_recoverable_iokit_error(kIOReturnSuccess));
        // Unrelated system namespace.
        assert!(!is_recoverable_iokit_error(0x10000000));
        // Arbitrary non-iokit negative value.
        assert!(!is_recoverable_iokit_error(-1));
    }
}
