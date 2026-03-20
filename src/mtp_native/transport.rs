use rusb::{Context, DeviceHandle, UsbContext};
use std::time::Duration;

/// USB interface class for MTP/PTP (Still Image Capture Device).
const PTP_USB_CLASS: u8 = 6;

/// Handles raw USB bulk transfers to/from the Zune's MTP interface.
pub struct UsbTransport {
    handle: DeviceHandle<Context>,
    ep_in: u8,
    ep_out: u8,
    iface: u8,
    timeout: Duration,
}

impl UsbTransport {
    /// Open the MTP interface on the given USB device.
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, String> {
        let context = Context::new().map_err(|e| format!("USB context: {e}"))?;
        let devices = context.devices().map_err(|e| format!("USB devices: {e}"))?;

        for device in devices.iter() {
            let desc = match device.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };
            if desc.vendor_id() != vendor_id || desc.product_id() != product_id {
                continue;
            }

            // Find the MTP interface and its bulk endpoints.
            // Try active config first, fall back to config index 0.
            let config = match device.active_config_descriptor() {
                Ok(c) => c,
                Err(_) => match device.config_descriptor(0) {
                    Ok(c) => c,
                    Err(e) => return Err(format!("Cannot read USB config: {e}")),
                },
            };

            for iface in config.interfaces() {
                for iface_desc in iface.descriptors() {
                    let class = iface_desc.class_code();
                    let subclass = iface_desc.sub_class_code();
                    let protocol = iface_desc.protocol_code();
                    eprintln!(
                        "  iface {} class={} subclass={} protocol={} endpoints={}",
                        iface_desc.interface_number(),
                        class,
                        subclass,
                        protocol,
                        iface_desc.num_endpoints(),
                    );

                    let mut ep_in = None;
                    let mut ep_out = None;

                    for ep in iface_desc.endpoint_descriptors() {
                        use rusb::TransferType;
                        if ep.transfer_type() != TransferType::Bulk {
                            continue;
                        }
                        use rusb::Direction;
                        match ep.direction() {
                            Direction::In => {
                                ep_in = Some(ep.address());
                            }
                            Direction::Out => {
                                ep_out = Some(ep.address());
                            }
                        }
                    }

                    if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                        let handle = device.open().map_err(|e| {
                            format!("Failed to open USB device: {e}")
                        })?;

                        // Read USB OS descriptors (required by Zune before MTP).
                        read_os_descriptors(&handle);

                        // Set USB configuration (the Zune may be unconfigured on fresh plug).
                        // The C++ reference explicitly sets this before claiming interfaces.
                        let config_value = config.number();
                        if let Err(e) = handle.set_active_configuration(config_value) {
                            eprintln!("  set_active_configuration({}): {e} (continuing)", config_value);
                        }

                        let iface_num = iface_desc.interface_number();

                        // On macOS, detach kernel driver if attached.
                        if handle.kernel_driver_active(iface_num).unwrap_or(false) {
                            handle.detach_kernel_driver(iface_num).map_err(|e| {
                                format!("Failed to detach kernel driver: {e}")
                            })?;
                        }

                        handle.claim_interface(iface_num).map_err(|e| {
                            format!("Failed to claim interface {iface_num}: {e}")
                        })?;

                        return Ok(UsbTransport {
                            handle,
                            ep_in,
                            ep_out,
                            iface: iface_num,
                            timeout: Duration::from_secs(15),
                        });
                    }
                }
            }
        }

        Err("Could not find MTP interface on device".to_string())
    }

    /// Set the read/write timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Write raw bytes to the bulk OUT endpoint.
    pub fn write(&self, data: &[u8]) -> Result<usize, String> {
        self.handle
            .write_bulk(self.ep_out, data, self.timeout)
            .map_err(|e| format!("USB write: {e}"))
    }

    /// Read raw bytes from the bulk IN endpoint.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, String> {
        self.handle
            .read_bulk(self.ep_in, buf, self.timeout)
            .map_err(|e| format!("USB read: {e}"))
    }

    /// Read a full MTP response, handling the case where the response
    /// may arrive in multiple USB packets.
    pub fn read_container(&self) -> Result<Vec<u8>, String> {
        let mut buf = vec![0u8; 16384];
        let n = self.read(&mut buf)?;
        if n < 4 {
            return Err("Short USB read".to_string());
        }
        let expected_len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let mut data = buf[..n].to_vec();

        // Keep reading if we haven't received the full container.
        while data.len() < expected_len {
            let n = self.read(&mut buf)?;
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n]);
        }
        Ok(data)
    }
}

/// Read USB OS descriptors that the Zune expects before MTP communication.
/// This matches the android-file-transfer-linux probe sequence:
/// 1. Read OS String Descriptor (string index 0xEE)
/// 2. Read Extended Compat ID OS Feature Descriptor (vendor request)
fn read_os_descriptors(handle: &DeviceHandle<Context>) {
    let timeout = Duration::from_millis(1000);

    // 1. Read OS String Descriptor at index 0xEE.
    // USB control transfer: GET_DESCRIPTOR, type=STRING (0x03), index=0xEE
    let mut buf = [0u8; 64];
    let request_type = 0x80; // Device-to-host, Standard, Device
    let request = 0x06; // GET_DESCRIPTOR
    let value = 0x03EE; // String descriptor, index 0xEE
    let index = 0x0000;
    match handle.read_control(request_type, request, value, index, &mut buf, timeout) {
        Ok(n) => {
            eprintln!("  OS String Descriptor: {} bytes", n);
            // Extract vendor code from the descriptor (byte at offset 16).
            if n >= 18 {
                let vendor_code = buf[16];
                eprintln!("  Vendor code: 0x{:02x}", vendor_code);

                // 2. Read Extended Compat ID descriptor using the vendor code.
                // USB control transfer: vendor request, device-to-host
                let request_type2 = 0xC0; // Device-to-host, Vendor, Device
                let mut buf2 = [0u8; 256];
                match handle.read_control(
                    request_type2,
                    vendor_code,
                    0x0000,
                    0x0004, // Extended Compat ID page
                    &mut buf2,
                    timeout,
                ) {
                    Ok(n2) => {
                        eprintln!("  Extended Compat ID: {} bytes", n2);
                        if n2 >= 24 {
                            let compat_id = std::str::from_utf8(&buf2[16..24])
                                .unwrap_or("?")
                                .trim_end_matches('\0');
                            eprintln!("  Compat ID: {}", compat_id);
                        }
                    }
                    Err(e) => eprintln!("  Extended Compat ID read failed: {e}"),
                }
            }
        }
        Err(e) => eprintln!("  OS String Descriptor read failed: {e}"),
    }
}

impl Drop for UsbTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}
