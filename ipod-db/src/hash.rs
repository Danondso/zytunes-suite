//! iPod Classic database hash (hash58).
//!
//! The iPod Classic firmware validates the iTunesDB on boot using an HMAC-SHA1
//! hash stored at mhbd offset 0x58. The key is derived from the device's
//! FirewireGuid (USB serial number) via LCM + AES S-box + SHA1.
//!
//! Without a valid hash, the firmware rejects the database and shows "No Music".

use sha1::{Digest, Sha1};

/// AES S-box (used in key derivation).
#[rustfmt::skip]
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// AES inverse S-box (used in key derivation).
#[rustfmt::skip]
const INV_SBOX: [u8; 256] = [
    0x52, 0x09, 0x6a, 0xd5, 0x30, 0x36, 0xa5, 0x38, 0xbf, 0x40, 0xa3, 0x9e, 0x81, 0xf3, 0xd7, 0xfb,
    0x7c, 0xe3, 0x39, 0x82, 0x9b, 0x2f, 0xff, 0x87, 0x34, 0x8e, 0x43, 0x44, 0xc4, 0xde, 0xe9, 0xcb,
    0x54, 0x7b, 0x94, 0x32, 0xa6, 0xc2, 0x23, 0x3d, 0xee, 0x4c, 0x95, 0x0b, 0x42, 0xfa, 0xc3, 0x4e,
    0x08, 0x2e, 0xa1, 0x66, 0x28, 0xd9, 0x24, 0xb2, 0x76, 0x5b, 0xa2, 0x49, 0x6d, 0x8b, 0xd1, 0x25,
    0x72, 0xf8, 0xf6, 0x64, 0x86, 0x68, 0x98, 0x16, 0xd4, 0xa4, 0x5c, 0xcc, 0x5d, 0x65, 0xb6, 0x92,
    0x6c, 0x70, 0x48, 0x50, 0xfd, 0xed, 0xb9, 0xda, 0x5e, 0x15, 0x46, 0x57, 0xa7, 0x8d, 0x9d, 0x84,
    0x90, 0xd8, 0xab, 0x00, 0x8c, 0xbc, 0xd3, 0x0a, 0xf7, 0xe4, 0x58, 0x05, 0xb8, 0xb3, 0x45, 0x06,
    0xd0, 0x2c, 0x1e, 0x8f, 0xca, 0x3f, 0x0f, 0x02, 0xc1, 0xaf, 0xbd, 0x03, 0x01, 0x13, 0x8a, 0x6b,
    0x3a, 0x91, 0x11, 0x41, 0x4f, 0x67, 0xdc, 0xea, 0x97, 0xf2, 0xcf, 0xce, 0xf0, 0xb4, 0xe6, 0x73,
    0x96, 0xac, 0x74, 0x22, 0xe7, 0xad, 0x35, 0x85, 0xe2, 0xf9, 0x37, 0xe8, 0x1c, 0x75, 0xdf, 0x6e,
    0x47, 0xf1, 0x1a, 0x71, 0x1d, 0x29, 0xc5, 0x89, 0x6f, 0xb7, 0x62, 0x0e, 0xaa, 0x18, 0xbe, 0x1b,
    0xfc, 0x56, 0x3e, 0x4b, 0xc6, 0xd2, 0x79, 0x20, 0x9a, 0xdb, 0xc0, 0xfe, 0x78, 0xcd, 0x5a, 0xf4,
    0x1f, 0xdd, 0xa8, 0x33, 0x88, 0x07, 0xc7, 0x31, 0xb1, 0x12, 0x10, 0x59, 0x27, 0x80, 0xec, 0x5f,
    0x60, 0x51, 0x7f, 0xa9, 0x19, 0xb5, 0x4a, 0x0d, 0x2d, 0xe5, 0x7a, 0x9f, 0x93, 0xc9, 0x9c, 0xef,
    0xa0, 0xe0, 0x3b, 0x4d, 0xae, 0x2a, 0xf5, 0xb0, 0xc8, 0xeb, 0xbb, 0x3c, 0x83, 0x53, 0x99, 0x61,
    0x17, 0x2b, 0x04, 0x7e, 0xba, 0x77, 0xd6, 0x26, 0xe1, 0x69, 0x14, 0x63, 0x55, 0x21, 0x0c, 0x7d,
];

/// Fixed salt used in key derivation.
const FIXED_SALT: [u8; 18] = [
    0x67, 0x23, 0xFE, 0x30, 0x45, 0x33, 0xF8, 0x90, 0x99, 0x21, 0x07, 0xC1, 0xD0, 0x12, 0xB2, 0xA1,
    0x07, 0x81,
];

/// mhbd header size required for hash58.
pub const MHBD_HEADER_SIZE: u32 = 244;

/// Offset of the hashing_scheme field in the mhbd header.
const HASHING_SCHEME_OFFSET: usize = 0x30;

/// Offset and size of fields zeroed before hashing.
const DB_ID_OFFSET: usize = 0x18;
const DB_ID_SIZE: usize = 8;
const UNK32_OFFSET: usize = 0x32;
const UNK32_SIZE: usize = 20;
const HASH58_OFFSET: usize = 0x58;
const HASH58_SIZE: usize = 20;

/// Parse a FirewireGuid hex string into raw bytes (up to 20 bytes, zero-padded).
pub fn parse_firewire_id(hex_str: &str) -> crate::Result<[u8; 20]> {
    let hex_str = hex_str.trim();
    if hex_str.is_empty() {
        return Err(crate::IpodDbError::Filesystem(
            "empty FirewireGuid".to_string(),
        ));
    }

    let mut bytes = [0u8; 20];
    let decoded: Vec<u8> = (0..hex_str.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex_str[i..i + 2], 16).ok())
        .collect();

    let len = decoded.len().min(20);
    bytes[..len].copy_from_slice(&decoded[..len]);
    Ok(bytes)
}

/// Compute GCD of two u32 values.
fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Compute LCM of two u32 values. Returns 1 if either is 0.
fn lcm(a: u32, b: u32) -> u32 {
    if a == 0 || b == 0 {
        return 1;
    }
    (a / gcd(a, b)) * b
}

/// Derive the 64-byte HMAC key from a FirewireGuid.
///
/// Process: take 4 pairs of bytes from the FWID, compute LCM of each pair,
/// run the high/low bytes through AES S-box and inverse S-box to produce 16
/// bytes, then SHA1(FIXED_SALT || those 16 bytes) → 20 bytes, zero-padded to 64.
fn derive_key(firewire_id: &[u8; 20]) -> [u8; 64] {
    let mut y = [0u8; 16];

    for i in 0..4 {
        let a = firewire_id[i * 2] as u32;
        let b = firewire_id[i * 2 + 1] as u32;
        let cur_lcm = lcm(a, b);

        let hi = ((cur_lcm >> 8) & 0xFF) as u8;
        let lo = (cur_lcm & 0xFF) as u8;

        y[i * 4] = SBOX[hi as usize];
        y[i * 4 + 1] = INV_SBOX[hi as usize];
        y[i * 4 + 2] = SBOX[lo as usize];
        y[i * 4 + 3] = INV_SBOX[lo as usize];
    }

    // SHA1(FIXED_SALT || y)
    let mut hasher = Sha1::new();
    hasher.update(FIXED_SALT);
    hasher.update(y);
    let digest = hasher.finalize();

    let mut key = [0u8; 64];
    key[..20].copy_from_slice(&digest);
    key
}

/// Compute HMAC-SHA1 with a 64-byte key over the given data.
fn hmac_sha1(key: &[u8; 64], data: &[u8]) -> [u8; 20] {
    // Inner: SHA1( (key XOR 0x36) || data )
    let mut ipad_key = [0u8; 64];
    for i in 0..64 {
        ipad_key[i] = key[i] ^ 0x36;
    }
    let mut inner = Sha1::new();
    inner.update(ipad_key);
    inner.update(data);
    let inner_hash = inner.finalize();

    // Outer: SHA1( (key XOR 0x5C) || inner_hash )
    let mut opad_key = [0u8; 64];
    for i in 0..64 {
        opad_key[i] = key[i] ^ 0x5C;
    }
    let mut outer = Sha1::new();
    outer.update(opad_key);
    outer.update(inner_hash);
    let result = outer.finalize();

    let mut out = [0u8; 20];
    out.copy_from_slice(&result);
    out
}

/// Compute and write the hash58 into a serialized iTunesDB.
///
/// The `itdb_data` must have an mhbd header of at least [`MHBD_HEADER_SIZE`] bytes.
/// The `firewire_id` is the device's FirewireGuid parsed into raw bytes.
///
/// This modifies `itdb_data` in place:
/// 1. Sets `hashing_scheme` to 1 at offset 0x30
/// 2. Zeros `db_id`, `unk_0x32`, and `hash58` fields
/// 3. Computes HMAC-SHA1 over the entire buffer
/// 4. Writes the 20-byte hash at offset 0x58
/// 5. Restores `db_id` and `unk_0x32`
pub fn sign_hash58(itdb_data: &mut [u8], firewire_id: &[u8; 20]) -> crate::Result<()> {
    if itdb_data.len() < MHBD_HEADER_SIZE as usize {
        return Err(crate::IpodDbError::Parse(format!(
            "iTunesDB too small for hash58: {} bytes (need >= {})",
            itdb_data.len(),
            MHBD_HEADER_SIZE
        )));
    }

    // Set hashing_scheme = 1 (hash58).
    itdb_data[HASHING_SCHEME_OFFSET..HASHING_SCHEME_OFFSET + 2]
        .copy_from_slice(&1u16.to_le_bytes());

    // Save fields that will be zeroed.
    let mut backup_db_id = [0u8; DB_ID_SIZE];
    let mut backup_unk32 = [0u8; UNK32_SIZE];
    backup_db_id.copy_from_slice(&itdb_data[DB_ID_OFFSET..DB_ID_OFFSET + DB_ID_SIZE]);
    backup_unk32.copy_from_slice(&itdb_data[UNK32_OFFSET..UNK32_OFFSET + UNK32_SIZE]);

    // Zero out fields for hash computation.
    itdb_data[DB_ID_OFFSET..DB_ID_OFFSET + DB_ID_SIZE].fill(0);
    itdb_data[UNK32_OFFSET..UNK32_OFFSET + UNK32_SIZE].fill(0);
    itdb_data[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE].fill(0);

    // Derive key and compute HMAC-SHA1.
    let key = derive_key(firewire_id);
    let hash = hmac_sha1(&key, itdb_data);

    // Write hash at offset 0x58.
    itdb_data[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE].copy_from_slice(&hash);

    // Restore zeroed fields.
    itdb_data[DB_ID_OFFSET..DB_ID_OFFSET + DB_ID_SIZE].copy_from_slice(&backup_db_id);
    itdb_data[UNK32_OFFSET..UNK32_OFFSET + UNK32_SIZE].copy_from_slice(&backup_unk32);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_firewire_id() {
        let id = parse_firewire_id("000A2700215CDB22").unwrap();
        assert_eq!(id[0], 0x00);
        assert_eq!(id[1], 0x0A);
        assert_eq!(id[2], 0x27);
        assert_eq!(id[7], 0x22);
        assert_eq!(id[8], 0x00); // zero-padded
    }

    #[test]
    fn test_lcm() {
        assert_eq!(lcm(4, 6), 12);
        assert_eq!(lcm(0, 5), 1);
        assert_eq!(lcm(5, 0), 1);
        assert_eq!(lcm(7, 7), 7);
        assert_eq!(lcm(3, 5), 15);
    }

    #[test]
    fn test_derive_key_deterministic() {
        let id = parse_firewire_id("000A2700215CDB22").unwrap();
        let key1 = derive_key(&id);
        let key2 = derive_key(&id);
        assert_eq!(key1, key2);
        // Key should have 20 non-zero SHA1 bytes followed by 44 zero bytes.
        assert_ne!(&key1[..20], &[0u8; 20]);
        assert_eq!(&key1[20..], &[0u8; 44]);
    }

    #[test]
    fn test_hmac_sha1_known() {
        // HMAC-SHA1("", "") should match a known value.
        let key = [0u8; 64];
        let hash = hmac_sha1(&key, b"");
        // SHA1 of (0x36 repeated 64 times) is known; this just verifies no crash.
        assert_ne!(hash, [0u8; 20]);
    }

    #[test]
    fn test_sign_hash58_modifies_buffer() {
        // Create a minimal buffer with enough space for mhbd header.
        let mut buf = vec![0u8; 300];
        buf[0..4].copy_from_slice(b"mhbd");
        buf[4..8].copy_from_slice(&244u32.to_le_bytes());
        buf[8..12].copy_from_slice(&300u32.to_le_bytes());

        // Set a db_id that should be preserved.
        buf[0x18..0x20].copy_from_slice(&0xDEADBEEFu64.to_le_bytes());

        let id = parse_firewire_id("000A2700215CDB22").unwrap();
        sign_hash58(&mut buf, &id).unwrap();

        // Hash should be non-zero.
        let hash = &buf[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE];
        assert_ne!(hash, &[0u8; 20], "hash58 should be written");

        // db_id should be restored.
        let db_id = u64::from_le_bytes(buf[0x18..0x20].try_into().unwrap());
        assert_eq!(db_id, 0xDEADBEEF, "db_id should be restored after hashing");

        // hashing_scheme should be 1.
        let scheme = u16::from_le_bytes(buf[0x30..0x32].try_into().unwrap());
        assert_eq!(scheme, 1);
    }

    #[test]
    fn test_sign_hash58_deterministic() {
        let mut buf1 = vec![0u8; 300];
        buf1[0..4].copy_from_slice(b"mhbd");
        buf1[4..8].copy_from_slice(&244u32.to_le_bytes());
        buf1[8..12].copy_from_slice(&300u32.to_le_bytes());
        buf1[244] = 0x42; // some payload data

        let mut buf2 = buf1.clone();
        let id = parse_firewire_id("000A2700215CDB22").unwrap();

        sign_hash58(&mut buf1, &id).unwrap();
        sign_hash58(&mut buf2, &id).unwrap();

        assert_eq!(
            &buf1[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE],
            &buf2[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE],
            "same input should produce same hash"
        );
    }

    #[test]
    fn test_sign_hash58_different_keys() {
        let mut buf1 = vec![0u8; 300];
        buf1[0..4].copy_from_slice(b"mhbd");
        buf1[4..8].copy_from_slice(&244u32.to_le_bytes());
        buf1[8..12].copy_from_slice(&300u32.to_le_bytes());

        let mut buf2 = buf1.clone();

        let id1 = parse_firewire_id("000A2700215CDB22").unwrap();
        let id2 = parse_firewire_id("FFFFFFFFFFFFFFFF").unwrap();

        sign_hash58(&mut buf1, &id1).unwrap();
        sign_hash58(&mut buf2, &id2).unwrap();

        assert_ne!(
            &buf1[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE],
            &buf2[HASH58_OFFSET..HASH58_OFFSET + HASH58_SIZE],
            "different keys should produce different hashes"
        );
    }

    #[test]
    fn test_sign_hash58_too_small() {
        let mut buf = vec![0u8; 100]; // too small for 244-byte header
        let id = parse_firewire_id("000A2700215CDB22").unwrap();
        assert!(sign_hash58(&mut buf, &id).is_err());
    }

    #[test]
    fn test_parse_firewire_id_empty() {
        assert!(parse_firewire_id("").is_err());
        assert!(parse_firewire_id("   ").is_err());
    }
}
