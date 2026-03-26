//! IOKit-based USB transport for MTP on macOS.
//!
//! Replaces the rusb/libusb transport that fails on data-out operations.
//! Uses IOUSBDeviceInterface/IOUSBInterfaceInterface directly via FFI.

use crate::iokit_ffi::*;
use std::ffi::CString;
use std::os::raw::c_void;

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
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, String> {
        unsafe { Self::open_inner(vendor_id, product_id) }
    }

    unsafe fn open_inner(vendor_id: u16, product_id: u16) -> Result<Self, String> {
        // Find the USB device via IOKit service matching.
        let class_name = CString::new(kUSBDeviceClassName).unwrap();
        let matching = IOServiceMatching(class_name.as_ptr());
        if matching.is_null() {
            return Err("IOServiceMatching failed".to_string());
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
        CFDictionarySetValue(matching, vendor_key as *const c_void, vendor_num as *const c_void);
        CFDictionarySetValue(matching, product_key as *const c_void, product_num as *const c_void);
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
            return Err(format!("IOServiceGetMatchingServices failed: {kr}"));
        }

        let service = IOIteratorNext(iterator);
        IOObjectRelease(iterator);
        if service == 0 {
            return Err(format!(
                "No USB device found with vid=0x{vendor_id:04x} pid=0x{product_id:04x}"
            ));
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
            return Err(format!("IOCreatePlugInInterfaceForService failed: {kr}"));
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
            return Err(format!("QueryInterface for device failed: {hr}"));
        }

        // Open the device.
        let kr = ((**device).USBDeviceOpen)(device);
        if kr != kIOReturnSuccess {
            ((**device).Release)(device);
            return Err(format!("USBDeviceOpen failed: 0x{kr:08x}"));
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
            return Err(format!("CreateInterfaceIterator failed: 0x{kr:08x}"));
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
            let kr = ((**device).CreateInterfaceIterator)(device, &request_any, &mut iface_iterator2);
            if kr != kIOReturnSuccess {
                ((**device).USBDeviceClose)(device);
                ((**device).Release)(device);
                return Err("No USB interfaces found on device".to_string());
            }
            let iface_service2 = IOIteratorNext(iface_iterator2);
            IOObjectRelease(iface_iterator2);
            if iface_service2 == 0 {
                ((**device).USBDeviceClose)(device);
                ((**device).Release)(device);
                return Err("No USB interfaces found on device".to_string());
            }
            return Self::open_interface(device, iface_service2);
        }

        Self::open_interface(device, iface_service)
    }

    unsafe fn open_interface(
        device: *mut *mut IOUSBDeviceInterface,
        iface_service: io_service_t,
    ) -> Result<Self, String> {
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
            return Err(format!("IOCreatePlugInInterfaceForService (interface) failed: {kr}"));
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
            return Err(format!("QueryInterface for interface failed: {hr}"));
        }

        // Open the interface.
        let kr = ((**interface).USBInterfaceOpen)(interface);
        if kr != kIOReturnSuccess {
            ((**interface).Release)(interface);
            ((**device).USBDeviceClose)(device);
            ((**device).Release)(device);
            return Err(format!("USBInterfaceOpen failed: 0x{kr:08x}"));
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
            return Err("Could not find bulk IN/OUT endpoints".to_string());
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
    pub fn write(&self, data: &[u8]) -> Result<usize, String> {
        let chunk_size = self.max_packet_size as usize;
        let mut offset = 0;
        // SAFETY: We hold valid IOKit interface pointers.
        unsafe {
            while offset < data.len() {
                let end = (offset + chunk_size).min(data.len());
                let chunk = &data[offset..end];
                let kr = ((**self.interface).WritePipe)(
                    self.interface,
                    self.pipe_out,
                    chunk.as_ptr() as *const c_void,
                    chunk.len() as UInt32,
                );
                if kr != kIOReturnSuccess {
                    return Err(format!("WritePipe failed: 0x{kr:08x}"));
                }
                offset = end;
            }
        }
        Ok(data.len())
    }

    /// Read data from the bulk IN endpoint.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, String> {
        // SAFETY: We hold valid IOKit interface pointers.
        unsafe {
            let mut size = buf.len() as UInt32;
            let kr = ((**self.interface).ReadPipe)(
                self.interface,
                self.pipe_in,
                buf.as_mut_ptr() as *mut c_void,
                &mut size,
            );
            if kr != kIOReturnSuccess {
                return Err(format!("ReadPipe failed: 0x{kr:08x}"));
            }
            Ok(size as usize)
        }
    }

    /// Read with a timeout (in seconds). Uses a background thread since
    /// the base IOUSBInterfaceInterface100 doesn't have ReadPipeTO.
    pub fn read_with_timeout(&self, buf: &mut [u8], timeout_secs: u64) -> Result<usize, String> {
        use std::sync::{Arc, Mutex};

        // SAFETY: The interface pointer remains valid for the duration of the read,
        // and we join the thread before returning. Cast to usize to cross Send boundary.
        let buf_len = buf.len();
        let shared_buf = Arc::new(Mutex::new(vec![0u8; buf_len]));
        let result = Arc::new(Mutex::new(None::<Result<usize, String>>));

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
                ((**iface).ReadPipe)(
                    iface,
                    pipe,
                    locked.as_mut_ptr() as *mut c_void,
                    &mut size,
                )
            };
            let r = if kr != kIOReturnSuccess {
                Err(format!("ReadPipe failed: 0x{kr:08x}"))
            } else {
                Ok(size as usize)
            };
            *result_clone.lock().unwrap() = Some(r);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            if std::time::Instant::now() > deadline {
                // Abort the pipe to unblock the read thread.
                unsafe {
                    ((**self.interface).AbortPipe)(self.interface, self.pipe_in);
                }
                let _ = handle.join();
                return Err(format!("ReadPipe timed out ({timeout_secs}s)"));
            }
            if let Some(r) = result.lock().unwrap().take() {
                let _ = handle.join();
                if let Ok(n) = &r {
                    let locked = shared_buf.lock().unwrap();
                    buf[..*n].copy_from_slice(&locked[..*n]);
                }
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// Read a full MTP container, reassembling multi-packet responses.
    pub fn read_container(&self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; 16384];
        let n = self.read_with_timeout(&mut buf, 30)?;
        if n < 4 {
            return Err("Short USB read".to_string());
        }
        let expected_len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let mut data = buf[..n].to_vec();

        while data.len() < expected_len {
            let n = self.read_with_timeout(&mut buf, 30)?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
        }
        Ok(data)
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
