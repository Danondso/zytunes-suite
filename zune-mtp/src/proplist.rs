//! MTP ObjectPropList builder for SendObjectPropList (0x9808).
//!
//! Binary format:
//! [u32 count] [properties...]
//!
//! Each property:
//! [u32 object_handle] [u16 prop_code] [u16 data_type] [value]

// MTP data type codes.
const DATATYPE_UINT16: u16 = 0x0004;
const DATATYPE_UINT32: u16 = 0x0006;
const DATATYPE_STRING: u16 = 0xFFFF;

// MTP object property codes.
pub const PROP_OBJECT_FILENAME: u16 = 0xDC07;
pub const PROP_NAME: u16 = 0xDC44;
pub const PROP_ARTIST: u16 = 0xDC46;
pub const PROP_TRACK: u16 = 0xDC8B;
pub const PROP_GENRE: u16 = 0xDC8C;
pub const PROP_ARTIST_ID: u16 = 0xDAB9;
pub const PROP_DATE_AUTHORED: u16 = 0xDC47;
pub const PROP_REPRESENTATIVE_SAMPLE_DATA: u16 = 0xDC86;

/// Builder for MTP property lists used with SendObjectPropList.
pub struct PropListBuilder {
    data: Vec<u8>,
    count: u32,
}

impl PropListBuilder {
    /// Create an empty property list builder.
    pub fn new() -> Self {
        PropListBuilder {
            data: Vec::new(),
            count: 0,
        }
    }

    /// Add a UTF-16LE string property.
    pub fn add_string(&mut self, prop_code: u16, value: &str) -> &mut Self {
        self.write_header(prop_code, DATATYPE_STRING);
        self.write_mtp_string(value);
        self.count += 1;
        self
    }

    /// Add a u16 property.
    pub fn add_u16(&mut self, prop_code: u16, value: u16) -> &mut Self {
        self.write_header(prop_code, DATATYPE_UINT16);
        self.data.extend_from_slice(&value.to_le_bytes());
        self.count += 1;
        self
    }

    /// Add a u32 property.
    pub fn add_u32(&mut self, prop_code: u16, value: u32) -> &mut Self {
        self.write_header(prop_code, DATATYPE_UINT32);
        self.data.extend_from_slice(&value.to_le_bytes());
        self.count += 1;
        self
    }

    /// Build the final binary payload.
    pub fn build(&self) -> Vec<u8> {
        let mut result = Vec::with_capacity(4 + self.data.len());
        result.extend_from_slice(&self.count.to_le_bytes());
        result.extend_from_slice(&self.data);
        result
    }

    fn write_header(&mut self, prop_code: u16, data_type: u16) {
        // Object handle = 0 for new objects.
        self.data.extend_from_slice(&0u32.to_le_bytes());
        self.data.extend_from_slice(&prop_code.to_le_bytes());
        self.data.extend_from_slice(&data_type.to_le_bytes());
    }

    /// Write a null-terminated UTF-16LE string in MTP format.
    /// Format: [u8 num_chars_including_null] [u16 chars...] [u16 null]
    fn write_mtp_string(&mut self, s: &str) {
        let chars: Vec<u16> = s.encode_utf16().collect();
        debug_assert!(chars.len() < 255, "MTP string too long ({} chars), would truncate on cast to u8", chars.len());
        let num_chars = (chars.len() + 1) as u8; // +1 for null terminator
        self.data.push(num_chars);
        for ch in &chars {
            self.data.extend_from_slice(&ch.to_le_bytes());
        }
        self.data.extend_from_slice(&0u16.to_le_bytes()); // null terminator
    }
}

impl Default for PropListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_proplist() {
        let pl = PropListBuilder::new().build();
        assert_eq!(pl, [0, 0, 0, 0]); // count = 0
    }

    #[test]
    fn single_u32_property() {
        let pl = PropListBuilder::new().add_u32(PROP_ARTIST_ID, 42).build();
        // count(4) + handle(4) + prop(2) + type(2) + value(4) = 16
        assert_eq!(pl.len(), 16);
        assert_eq!(u32::from_le_bytes(pl[0..4].try_into().unwrap()), 1); // count
        assert_eq!(u32::from_le_bytes(pl[4..8].try_into().unwrap()), 0); // handle
        assert_eq!(u16::from_le_bytes(pl[8..10].try_into().unwrap()), PROP_ARTIST_ID);
        assert_eq!(u16::from_le_bytes(pl[10..12].try_into().unwrap()), DATATYPE_UINT32);
        assert_eq!(u32::from_le_bytes(pl[12..16].try_into().unwrap()), 42);
    }

    #[test]
    fn single_string_property() {
        let pl = PropListBuilder::new().add_string(PROP_NAME, "Hi").build();
        // count(4) + handle(4) + prop(2) + type(2) + num_chars(1) + "H\0i\0\0\0" = 19
        assert_eq!(u32::from_le_bytes(pl[0..4].try_into().unwrap()), 1);
        // num_chars = 3 (H, i, null) — stored as u8
        assert_eq!(pl[12], 3);
    }

    #[test]
    fn multiple_properties() {
        let pl = PropListBuilder::new()
            .add_string(PROP_NAME, "Song")
            .add_u16(PROP_TRACK, 5)
            .add_u32(PROP_ARTIST_ID, 100)
            .build();
        assert_eq!(u32::from_le_bytes(pl[0..4].try_into().unwrap()), 3); // count
    }
}
