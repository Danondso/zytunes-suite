//! Platform-specific USB transport backends for MTP communication.
//!
//! On macOS, uses IOKit FFI for bulk I/O (libusb fails on data-out operations).
//! On Linux, uses rusb (libusb wrapper) which handles bulk I/O natively.

use crate::MtpError;

#[cfg(target_os = "macos")]
mod iokit;
#[cfg(target_os = "macos")]
pub(crate) mod iokit_ffi;
#[cfg(target_os = "macos")]
pub use iokit::IokitTransport as Transport;

#[cfg(target_os = "linux")]
mod libusb;
#[cfg(target_os = "linux")]
pub use self::libusb::LibusbTransport as Transport;

/// Reassemble a full MTP container from a reader function.
///
/// The reader is called repeatedly to fill a buffer. The first 4 bytes of the
/// first read are the little-endian container length. Subsequent reads are
/// appended until the full container is received or the reader returns 0 bytes.
pub(crate) fn reassemble_container<F>(mut read_fn: F) -> Result<Vec<u8>, MtpError>
where
    F: FnMut(&mut [u8]) -> Result<usize, MtpError>,
{
    let mut buf = vec![0u8; 16384];
    let n = read_fn(&mut buf)?;
    if n < 4 {
        return Err(MtpError::Usb("Short USB read".to_string()));
    }
    let expected_len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let mut data = buf[..n].to_vec();

    while data.len() < expected_len {
        let n = read_fn(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    Ok(data)
}

/// Split data into chunks of `chunk_size` and write each via the writer function.
///
/// Returns the total number of bytes written.
pub(crate) fn chunked_write<F>(
    data: &[u8],
    chunk_size: usize,
    mut write_fn: F,
) -> Result<usize, MtpError>
where
    F: FnMut(&[u8]) -> Result<usize, MtpError>,
{
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + chunk_size).min(data.len());
        let chunk = &data[offset..end];
        let n = write_fn(chunk)?;
        if n != chunk.len() {
            return Err(MtpError::Usb(format!(
                "Short write: expected {} bytes, wrote {n}",
                chunk.len()
            )));
        }
        offset = end;
    }
    Ok(data.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- reassemble_container tests ---

    #[test]
    fn reassemble_single_packet() {
        // Container with length=12, all data in one read.
        let container = {
            let mut v = vec![0u8; 12];
            v[..4].copy_from_slice(&12u32.to_le_bytes()); // length = 12
            v[4] = 0x02; // type = response
            v[6] = 0x01; // code low
            v[7] = 0x20; // code high
            v
        };
        let mut call_count = 0;
        let result = reassemble_container(|buf| {
            call_count += 1;
            if call_count == 1 {
                buf[..container.len()].copy_from_slice(&container);
                Ok(container.len())
            } else {
                Ok(0)
            }
        });
        assert_eq!(result.unwrap(), container);
    }

    #[test]
    fn reassemble_multi_packet() {
        // Container with length=20, split across two reads.
        let mut full = vec![0u8; 20];
        full[..4].copy_from_slice(&20u32.to_le_bytes());
        for (i, byte) in full.iter_mut().enumerate().skip(4) {
            *byte = i as u8;
        }

        let part1 = full[..10].to_vec();
        let part2 = full[10..].to_vec();

        let mut call_count = 0;
        let result = reassemble_container(|buf| {
            call_count += 1;
            match call_count {
                1 => {
                    buf[..part1.len()].copy_from_slice(&part1);
                    Ok(part1.len())
                }
                2 => {
                    buf[..part2.len()].copy_from_slice(&part2);
                    Ok(part2.len())
                }
                _ => Ok(0),
            }
        });
        assert_eq!(result.unwrap(), full);
        assert_eq!(call_count, 2);
    }

    #[test]
    fn reassemble_short_read_returns_error() {
        // First read returns only 3 bytes — too short for length header.
        let result = reassemble_container(|buf| {
            buf[0] = 0xFF;
            buf[1] = 0xFF;
            buf[2] = 0xFF;
            Ok(3)
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Short USB read"));
    }

    #[test]
    fn reassemble_reader_error_propagates() {
        let result =
            reassemble_container(|_buf| Err(MtpError::Usb("device disconnected".to_string())));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("device disconnected"));
    }

    #[test]
    fn reassemble_partial_container_on_eof() {
        // Container says length=100 but reader gives 20 bytes then EOF.
        let mut header = vec![0u8; 20];
        header[..4].copy_from_slice(&100u32.to_le_bytes());

        let mut call_count = 0;
        let result = reassemble_container(|buf| {
            call_count += 1;
            if call_count == 1 {
                buf[..header.len()].copy_from_slice(&header);
                Ok(header.len())
            } else {
                Ok(0) // EOF
            }
        });
        // Should return what we got, not error — the caller checks completeness.
        let data = result.unwrap();
        assert_eq!(data.len(), 20);
    }

    // --- chunked_write tests ---

    #[test]
    fn chunked_write_exact_multiple() {
        let data = vec![0xBB; 1024];
        let mut chunks_seen = 0;
        let total = chunked_write(&data, 512, |chunk| {
            assert_eq!(chunk.len(), 512);
            chunks_seen += 1;
            Ok(chunk.len())
        })
        .unwrap();
        assert_eq!(total, 1024);
        assert_eq!(chunks_seen, 2);
    }

    #[test]
    fn chunked_write_remainder_chunk() {
        let data = vec![0xCC; 700];
        let mut chunk_sizes = Vec::new();
        let total = chunked_write(&data, 512, |chunk| {
            chunk_sizes.push(chunk.len());
            Ok(chunk.len())
        })
        .unwrap();
        assert_eq!(total, 700);
        assert_eq!(chunk_sizes, vec![512, 188]);
    }

    #[test]
    fn chunked_write_empty_data() {
        let mut called = false;
        let total = chunked_write(&[], 512, |_chunk| {
            called = true;
            Ok(0)
        })
        .unwrap();
        assert_eq!(total, 0);
        assert!(!called);
    }

    #[test]
    fn chunked_write_short_write_returns_error() {
        let data = vec![0xDD; 100];
        let result = chunked_write(&data, 512, |_chunk| {
            Ok(50) // Only wrote half
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Short write"));
    }

    #[test]
    fn chunked_write_error_propagates() {
        let data = vec![0xEE; 100];
        let result = chunked_write(&data, 512, |_chunk| {
            Err(MtpError::Usb("pipe broken".to_string()))
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pipe broken"));
    }
}
