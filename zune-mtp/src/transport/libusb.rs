//! libusb-based USB transport for MTP on Linux.
//!
//! Uses the `rusb` crate (libusb wrapper) for bulk I/O. On Linux, libusb
//! talks directly to the kernel's USB subsystem and handles bulk-out
//! transfers natively — unlike macOS where IOKit is needed.

use crate::MtpError;
use rusb::{Error as RusbError, UsbContext};
use std::time::Duration;

/// True when a libusb error on a bulk endpoint is worth attempting
/// `clear_halt` recovery against — pipe stalls (`Pipe`) and transient
/// I/O or signalling hiccups (`Io`, `Interrupted`, `Overflow`) that
/// commonly wedge the pipe and are typically clearable. Terminal errors
/// (`NoDevice`, `Access`) still fail on the retry attempt and surface
/// both errors in the message.
fn is_recoverable_libusb_error(err: &RusbError) -> bool {
    matches!(
        err,
        RusbError::Pipe | RusbError::Io | RusbError::Interrupted | RusbError::Overflow
    )
}

/// USB transport using libusb (via rusb) for bulk I/O.
pub struct LibusbTransport {
    handle: rusb::DeviceHandle<rusb::Context>,
    endpoint_in: u8,
    endpoint_out: u8,
    max_packet_size: u16,
    interface_number: u8,
}

impl LibusbTransport {
    /// Open a USB device by vendor/product ID and claim its MTP interface.
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, MtpError> {
        let context =
            rusb::Context::new().map_err(|e| MtpError::Usb(format!("libusb init failed: {e}")))?;

        let devices = context
            .devices()
            .map_err(|e| MtpError::Usb(format!("Failed to list USB devices: {e}")))?;

        // Find the device by vendor/product ID.
        let device = devices
            .iter()
            .find(|dev| {
                dev.device_descriptor()
                    .map(|desc| desc.vendor_id() == vendor_id && desc.product_id() == product_id)
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                MtpError::Usb(format!(
                    "No USB device found with vid=0x{vendor_id:04x} pid=0x{product_id:04x}"
                ))
            })?;

        let handle = device
            .open()
            .map_err(|e| MtpError::Usb(format!("Failed to open USB device: {e}")))?;

        // Detach kernel driver if one is attached (e.g., usb-storage).
        // This is a no-op if no kernel driver is bound.
        let _ = handle.set_auto_detach_kernel_driver(true);

        // Reset the device to wake it from USB suspend.
        // On macOS, this is done via USBDeviceSuspend(dev, 0). On Linux,
        // a USB reset achieves the same effect — the Zune needs to be woken
        // before it will respond to MTP commands.
        let _ = handle.reset();

        // Set configuration (use config 1, matching IOKit backend).
        let _ = handle.set_active_configuration(1);

        // Probe OS descriptors to match aft-mtp-cli's initialization sequence.
        Self::probe_os_descriptors(&handle);

        // Find the MTP interface and its bulk endpoints.
        let config = device
            .active_config_descriptor()
            .map_err(|e| MtpError::Usb(format!("Failed to get config descriptor: {e}")))?;

        let mut interface_number = 0u8;
        let mut endpoint_in = 0u8;
        let mut endpoint_out = 0u8;
        let mut max_packet_size = 512u16;
        let mut found = false;

        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                // Look for PTP/MTP Still Image class (6/1/1) first.
                let is_mtp = desc.class_code() == 6
                    && desc.sub_class_code() == 1
                    && desc.protocol_code() == 1;

                if is_mtp || !found {
                    let mut ep_in = 0u8;
                    let mut ep_out = 0u8;
                    let mut mps = 512u16;

                    for ep in desc.endpoint_descriptors() {
                        if ep.transfer_type() == rusb::TransferType::Bulk {
                            match ep.direction() {
                                rusb::Direction::In if ep_in == 0 => {
                                    ep_in = ep.address();
                                    mps = ep.max_packet_size();
                                }
                                rusb::Direction::Out if ep_out == 0 => {
                                    ep_out = ep.address();
                                }
                                _ => {}
                            }
                        }
                    }

                    if ep_in != 0 && ep_out != 0 {
                        interface_number = desc.interface_number();
                        endpoint_in = ep_in;
                        endpoint_out = ep_out;
                        max_packet_size = mps;
                        found = true;
                        if is_mtp {
                            break; // Prefer the MTP-class interface.
                        }
                    }
                }
            }
            if found && endpoint_in != 0 {
                break;
            }
        }

        if endpoint_in == 0 || endpoint_out == 0 {
            return Err(MtpError::Usb(
                "Could not find bulk IN/OUT endpoints".to_string(),
            ));
        }

        // Claim the MTP interface.
        handle.claim_interface(interface_number).map_err(|e| {
            MtpError::Usb(format!("Failed to claim interface {interface_number}: {e}"))
        })?;

        Ok(LibusbTransport {
            handle,
            endpoint_in,
            endpoint_out,
            max_packet_size,
            interface_number,
        })
    }

    /// Probe USB OS String Descriptor and Extended Compat ID,
    /// matching aft-mtp-cli's initialization sequence.
    fn probe_os_descriptors(handle: &rusb::DeviceHandle<rusb::Context>) {
        let timeout = Duration::from_secs(2);

        // Read OS String Descriptor at index 0xEE.
        let mut buf = [0u8; 64];
        let result = handle.read_control(
            0x80,   // Device-to-host, Standard, Device
            0x06,   // GET_DESCRIPTOR
            0x03EE, // String descriptor, index 0xEE
            0x0000, &mut buf, timeout,
        );

        if let Ok(n) = result {
            if n >= 18 {
                let vendor_code = buf[16];
                // Read Extended Compat ID using vendor code.
                let mut buf2 = [0u8; 256];
                let _ = handle.read_control(
                    0xC0, // Device-to-host, Vendor, Device
                    vendor_code,
                    0x0000,
                    0x0004, // Extended Compat ID page
                    &mut buf2,
                    timeout,
                );
            }
        }
    }

    /// Write data to the bulk OUT endpoint.
    /// Writes in max-packet-size chunks, matching aft-mtp-cli behavior.
    pub fn write(&self, data: &[u8]) -> Result<usize, MtpError> {
        let timeout = Duration::from_secs(30);
        super::chunked_write(data, self.max_packet_size as usize, |chunk| {
            match self.handle.write_bulk(self.endpoint_out, chunk, timeout) {
                Ok(n) => Ok(n),
                Err(e) if is_recoverable_libusb_error(&e) => {
                    // Clear any host-side stall (prior ReadPipe timeouts, cable
                    // jostles, or Zune firmware hiccups can leave the OUT pipe
                    // wedged — every subsequent write then fails with the same
                    // error until the user physically replugs). Retry once.
                    let _ = self.handle.clear_halt(self.endpoint_out);
                    self.handle
                        .write_bulk(self.endpoint_out, chunk, timeout)
                        .map_err(|e2| {
                            MtpError::Usb(format!(
                                "write_bulk failed: {e} (retry after clear_halt: {e2})"
                            ))
                        })
                }
                Err(e) => Err(MtpError::Usb(format!("write_bulk failed: {e}"))),
            }
        })
    }

    /// Read data from the bulk IN endpoint.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, MtpError> {
        let timeout = Duration::from_secs(30);
        match self.handle.read_bulk(self.endpoint_in, buf, timeout) {
            Ok(n) => Ok(n),
            Err(e) if is_recoverable_libusb_error(&e) => {
                let _ = self.handle.clear_halt(self.endpoint_in);
                self.handle
                    .read_bulk(self.endpoint_in, buf, timeout)
                    .map_err(|e2| {
                        MtpError::Usb(format!(
                            "read_bulk failed: {e} (retry after clear_halt: {e2})"
                        ))
                    })
            }
            Err(e) => Err(MtpError::Usb(format!("read_bulk failed: {e}"))),
        }
    }

    /// Read with a timeout (in seconds).
    /// libusb supports native timeouts, so no background thread is needed.
    pub fn read_with_timeout(&self, buf: &mut [u8], timeout_secs: u64) -> Result<usize, MtpError> {
        let timeout = Duration::from_secs(timeout_secs);
        match self.handle.read_bulk(self.endpoint_in, buf, timeout) {
            Ok(n) => Ok(n),
            Err(RusbError::Timeout) => {
                // Mirror the IOKit path: on a read timeout, clear stalls on
                // BOTH bulk endpoints. If we only clear IN, the OUT pipe can
                // remain out of sync with the device (it's still waiting to
                // ACK data we gave up on) and every subsequent write fails
                // until the user physically replugs.
                let _ = self.handle.clear_halt(self.endpoint_in);
                let _ = self.handle.clear_halt(self.endpoint_out);
                Err(MtpError::Usb(format!(
                    "read_bulk timed out ({timeout_secs}s)"
                )))
            }
            Err(e) if is_recoverable_libusb_error(&e) => {
                let _ = self.handle.clear_halt(self.endpoint_in);
                self.handle
                    .read_bulk(self.endpoint_in, buf, timeout)
                    .map_err(|e2| {
                        MtpError::Usb(format!(
                            "read_bulk failed: {e} (retry after clear_halt: {e2})"
                        ))
                    })
            }
            Err(e) => Err(MtpError::Usb(format!("read_bulk failed: {e}"))),
        }
    }

    /// Read a full MTP container, reassembling multi-packet responses.
    pub fn read_container(&self) -> Result<Vec<u8>, MtpError> {
        self.read_container_with_timeout(30)
    }

    /// Like `read_container` but with a caller-supplied timeout in seconds.
    pub fn read_container_with_timeout(&self, timeout_secs: u64) -> Result<Vec<u8>, MtpError> {
        super::reassemble_container(|buf| self.read_with_timeout(buf, timeout_secs))
    }
}

impl Drop for LibusbTransport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface_number);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_recoverable_libusb_error_recognises_stall_and_transients() {
        // Pipe stall is the primary case we want to clear and retry.
        assert!(is_recoverable_libusb_error(&RusbError::Pipe));
        // Transient I/O / signalling hiccups seen during sync cascades.
        assert!(is_recoverable_libusb_error(&RusbError::Io));
        assert!(is_recoverable_libusb_error(&RusbError::Interrupted));
        assert!(is_recoverable_libusb_error(&RusbError::Overflow));
    }

    #[test]
    fn is_recoverable_libusb_error_rejects_terminal_conditions() {
        // Device physically gone — retry is pointless and misleading.
        assert!(!is_recoverable_libusb_error(&RusbError::NoDevice));
        // Permission problem — won't improve on retry.
        assert!(!is_recoverable_libusb_error(&RusbError::Access));
        // Timeout is handled separately (no retry, just clear both pipes).
        assert!(!is_recoverable_libusb_error(&RusbError::Timeout));
        // NotFound / InvalidParam indicate caller error, not a wire glitch.
        assert!(!is_recoverable_libusb_error(&RusbError::NotFound));
        assert!(!is_recoverable_libusb_error(&RusbError::InvalidParam));
    }
}
