use crate::container::OperationCode;
use crate::session::MtpSession;
use crate::MtpError;

use aes::Aes128;
use cipher::{BlockDecryptMut, KeyIvInit};
use cmac::{Cmac, Mac};
use num_bigint::BigUint;

use sha1::{Digest, Sha1};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// Cryptographic keys loaded from .mtpz-data.
pub struct MtpzKeys {
    pub public_exp: BigUint,
    pub modulus: BigUint,
    pub private_exp: BigUint,
    pub session_key: Vec<u8>,
    pub certificate: Vec<u8>,
}

impl MtpzKeys {
    /// Load keys from the .mtpz-data file format (5 hex lines).
    pub fn load(path: &str) -> Result<Self, MtpError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| MtpError::KeyLoad(format!("Could not read {path}: {e}")))?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < 5 {
            return Err(MtpError::KeyLoad(format!(
                "Expected 5 lines in .mtpz-data, got {}",
                lines.len()
            )));
        }

        let public_exp = BigUint::parse_bytes(lines[0].trim().as_bytes(), 16)
            .ok_or(MtpError::KeyLoad("Invalid public exponent".to_string()))?;
        let session_key = hex::decode(lines[1].trim())
            .map_err(|e| MtpError::KeyLoad(format!("Invalid session key hex: {e}")))?;
        let modulus = BigUint::parse_bytes(lines[2].trim().as_bytes(), 16)
            .ok_or(MtpError::KeyLoad("Invalid modulus".to_string()))?;
        let private_exp = BigUint::parse_bytes(lines[3].trim().as_bytes(), 16)
            .ok_or(MtpError::KeyLoad("Invalid private exponent".to_string()))?;
        let certificate = hex::decode(lines[4].trim())
            .map_err(|e| MtpError::KeyLoad(format!("Invalid certificate hex: {e}")))?;

        Ok(MtpzKeys {
            public_exp,
            modulus,
            private_exp,
            session_key,
            certificate,
        })
    }

    /// Load from the default location ~/.mtpz-data.
    pub fn load_default() -> Result<Self, MtpError> {
        let home = std::env::var("HOME").map_err(|_| MtpError::KeyLoad("HOME not set".to_string()))?;
        Self::load(&format!("{home}/.mtpz-data"))
    }

    fn rsa_size(&self) -> usize {
        (self.modulus.bits() as usize).div_ceil(8)
    }

    fn rsa_raw(&self, data: &[u8]) -> Vec<u8> {
        let m = BigUint::from_bytes_be(data);
        let result = m.modpow(&self.private_exp, &self.modulus);
        let result_bytes = result.to_bytes_be();
        let rsa_size = self.rsa_size();
        let mut padded = vec![0u8; rsa_size];
        let start = rsa_size.saturating_sub(result_bytes.len());
        padded[start..].copy_from_slice(&result_bytes[..rsa_size.min(result_bytes.len())]);
        padded
    }

    fn hkdf(message: &[u8], key_size: usize) -> Vec<u8> {
        let block_size = 20;
        let blocks = key_size.div_ceil(block_size);
        let mut key = Vec::with_capacity(blocks * block_size);
        let mut ctr_buf = Vec::with_capacity(message.len() + 4);
        ctr_buf.extend_from_slice(message);
        ctr_buf.extend_from_slice(&[0, 0, 0, 0]);
        let ctr_start = message.len();

        for i in 0..blocks {
            ctr_buf[ctr_start] = (i >> 24) as u8;
            ctr_buf[ctr_start + 1] = (i >> 16) as u8;
            ctr_buf[ctr_start + 2] = (i >> 8) as u8;
            ctr_buf[ctr_start + 3] = i as u8;
            let hash = Sha1::digest(&ctr_buf);
            key.extend_from_slice(&hash);
        }
        key
    }

    fn generate_certificate_message(&self) -> (Vec<u8>, Vec<u8>) {
        let rsa_size = self.rsa_size();
        let message_size = 156 + self.certificate.len();

        let mut challenge = vec![0u8; 16];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut challenge);

        let mut message = Vec::with_capacity(message_size);
        message.push(0x02);
        message.push(0x01);
        message.push(0x01);
        message.push(0x00);
        message.push(0x00);
        message.push((self.certificate.len() >> 8) as u8);
        message.push(self.certificate.len() as u8);
        message.extend_from_slice(&self.certificate);
        message.push((challenge.len() >> 8) as u8);
        message.push(challenge.len() as u8);
        message.extend_from_slice(&challenge);

        let inner_hash = Sha1::digest(&message[2..]);
        // The 8-byte zero prefix matches the MTPZ protocol spec's salt format:
        // 8 zero bytes followed by the SHA-1 hash of the message body.
        let mut salt = vec![0u8; 20 + 8];
        salt[8..].copy_from_slice(&inner_hash);
        let hash: Vec<u8> = Sha1::digest(&salt).to_vec();

        let key = Self::hkdf(&hash, 107);

        let mut signature = vec![0u8; rsa_size];
        signature[106] = 1;
        signature[107..(hash.len() + 107)].copy_from_slice(&hash);
        for i in 0..107 {
            signature[i] ^= key[i];
        }
        signature[0] &= 127;
        signature[rsa_size - 1] = 188;

        let signed = self.rsa_raw(&signature);

        message.push(0x01);
        message.push(0x00);
        message.push(signed.len() as u8);
        message.extend_from_slice(&signed);

        (challenge, message)
    }

    fn aes_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, MtpError> {
        if key.len() != 16 {
            return Err(MtpError::Crypto(format!("AES key must be 16 bytes, got {}", key.len())));
        }
        let iv = [0u8; 16];
        let mut buf = data.to_vec();
        Aes128CbcDec::new(key.into(), &iv.into())
            .decrypt_padded_mut::<cipher::block_padding::NoPadding>(&mut buf)
            .map_err(|e| MtpError::Crypto(format!("AES decrypt failed: {e}")))?;
        Ok(buf)
    }

    fn cmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>, MtpError> {
        let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key)
            .map_err(|e| MtpError::Crypto(format!("CMAC init: {e}")))?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn verify_response(
        &self,
        response: &[u8],
        original_challenge: &[u8],
    ) -> Result<Vec<u8>, MtpError> {
        let rsa_size = self.rsa_size();
        let mut pos = 0;

        if response.len() < 4 {
            return Err(MtpError::Crypto("Response too short".to_string()));
        }
        if response[pos] != 0x02 || response[pos + 1] != 0x02 {
            return Err(MtpError::Crypto(format!(
                "Invalid response tag: {:02x} {:02x}",
                response[pos],
                response[pos + 1]
            )));
        }
        pos += 2;

        let sig_size = ((response[pos] as usize) << 8) | (response[pos + 1] as usize);
        pos += 2;
        if sig_size < 0x80 || sig_size != rsa_size {
            return Err(MtpError::Crypto(format!("Invalid signature size: {sig_size}")));
        }

        let sig_data = &response[pos..pos + sig_size];
        let mut signature = self.rsa_raw(sig_data);
        pos += sig_size;

        {
            let hash = Self::hkdf(&signature[21..128], 20);
            for i in 0..20 {
                signature[1 + i] ^= hash[i];
            }
            let key = Self::hkdf(&signature[1..21], 107);
            for i in 0..107 {
                signature[21 + i] ^= key[i];
            }
        }

        let aes_key: Vec<u8> = signature[0x70..].to_vec();

        if pos + 4 > response.len() {
            return Err(MtpError::Crypto("Response too short for payload header".to_string()));
        }
        if response[pos] != 0 || response[pos + 1] != 0 {
            return Err(MtpError::Crypto("Invalid payload record".to_string()));
        }
        pos += 2;
        let payload_size = ((response[pos] as usize) << 8) | (response[pos + 1] as usize);
        pos += 2;

        if pos + payload_size > response.len() {
            return Err(MtpError::Crypto("Response too short for payload".to_string()));
        }

        let payload = Self::aes_decrypt(&aes_key, &response[pos..pos + payload_size])?;

        let mut pp = 0;
        if payload.is_empty() || payload[pp] != 1 {
            return Err(MtpError::Crypto("Decryption failed (bad payload marker)".to_string()));
        }
        pp += 1;

        if pp + 4 > payload.len() {
            return Err(MtpError::Crypto("Payload too short for cert size".to_string()));
        }
        let cert_size = ((payload[pp] as usize) << 24)
            | ((payload[pp + 1] as usize) << 16)
            | ((payload[pp + 2] as usize) << 8)
            | (payload[pp + 3] as usize);
        pp += 4;
        pp += cert_size;

        if pp + 2 > payload.len() {
            return Err(MtpError::Crypto("Payload too short for challenge size".to_string()));
        }
        let challenge_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        if challenge_size != original_challenge.len() {
            return Err(MtpError::Crypto(format!("Challenge size mismatch: {challenge_size}")));
        }
        if pp + challenge_size > payload.len() {
            return Err(MtpError::Crypto("Payload too short for challenge".to_string()));
        }
        if &payload[pp..pp + challenge_size] != original_challenge {
            return Err(MtpError::Crypto("Challenge does not match!".to_string()));
        }
        pp += challenge_size;

        if pp + 2 > payload.len() {
            return Err(MtpError::Crypto("Payload too short for device challenge".to_string()));
        }
        let dev_challenge_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        pp += dev_challenge_size;

        if pp + 3 > payload.len() {
            return Err(MtpError::Crypto("Payload too short for signature header".to_string()));
        }
        if payload[pp] != 1 {
            return Err(MtpError::Crypto(format!("Invalid signature marker: 0x{:02x} at pp={}", payload[pp], pp)));
        }
        pp += 1;
        let dev_sig_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        pp += dev_sig_size;

        if pp + 3 > payload.len() {
            return Err(MtpError::Crypto("Payload too short for CMAC header".to_string()));
        }
        if payload[pp] != 1 {
            return Err(MtpError::Crypto(format!("Invalid CMAC record marker: 0x{:02x} at pp={}", payload[pp], pp)));
        }
        pp += 1;
        let cmac_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        if pp + cmac_size > payload.len() {
            return Err(MtpError::Crypto("Payload too short for CMAC key".to_string()));
        }

        Ok(payload[pp..pp + cmac_size].to_vec())
    }

    fn sign_response(cmac_key: &[u8]) -> Result<Vec<u8>, MtpError> {
        let mut text = vec![0u8; 16];
        text[15] = 1;

        let mac = Self::cmac(&cmac_key[..16], &text)?;

        let mut message = Vec::with_capacity(20);
        message.push(0x02);
        message.push(0x03);
        message.push(0x00);
        message.push(0x10);
        message.extend_from_slice(&mac);
        Ok(message)
    }

    fn sign_session_request(cmac_key: &[u8]) -> Result<[u32; 4], MtpError> {
        let key = &cmac_key[..16];
        let data = if cmac_key.len() >= 20 {
            &cmac_key[16..20]
        } else {
            &[0u8; 4]
        };
        let signature = Self::cmac(key, data)?;

        let mut cmac = [0u32; 4];
        for i in 0..4 {
            cmac[i] = ((signature[i * 4] as u32) << 24)
                | ((signature[i * 4 + 1] as u32) << 16)
                | ((signature[i * 4 + 2] as u32) << 8)
                | (signature[i * 4 + 3] as u32);
        }
        Ok(cmac)
    }
}

/// Run the full MTPZ authentication handshake.
/// The `log` callback receives diagnostic messages for each step.
pub fn authenticate(
    session: &mut MtpSession,
    keys: &MtpzKeys,
    log: &dyn Fn(&str),
) -> Result<(), MtpError> {
    log("MTPZ: Setting session initiator...");
    let _ = session.set_device_prop_string(0xD406, "zune-mtp - MTPZClassDriver");

    log("MTPZ: Ending existing trusted session...");
    let _ = session.execute_simple(OperationCode::EndTrustedAppSession, &[]);

    log("MTPZ: Sending certificate...");
    let (challenge, cert_message) = keys.generate_certificate_message();
    session.generic_operation_send(OperationCode::SendWMDRMPDAppRequest, &cert_message, log)?;

    log("MTPZ: Getting device response...");
    let response =
        session.generic_operation_receive(OperationCode::GetWMDRMPDAppResponse, log)?;

    log("MTPZ: Verifying response...");
    let cmac_key = keys.verify_response(&response, &challenge)?;

    log("MTPZ: Sending confirmation...");
    let confirmation = MtpzKeys::sign_response(&cmac_key)?;
    session.generic_operation_send(OperationCode::SendWMDRMPDAppRequest, &confirmation, log)?;

    log("MTPZ: Enabling secure file operations...");

    let cmac_values = MtpzKeys::sign_session_request(&cmac_key)?;
    session.enable_secure_file_operations(cmac_values)?;
    log("MTPZ: Secure file operations enabled!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_keys_from_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        // 5 hex lines: public_exp, session_key, modulus, private_exp, certificate
        writeln!(tmp, "10001").unwrap();
        writeln!(tmp, "0102030405060708090a0b0c0d0e0f10").unwrap();
        writeln!(tmp, "DEADBEEF").unwrap();
        writeln!(tmp, "CAFEBABE").unwrap();
        writeln!(tmp, "FF00FF00").unwrap();
        tmp.flush().unwrap();

        let keys = MtpzKeys::load(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(keys.public_exp, BigUint::from(0x10001u32));
        assert_eq!(keys.session_key, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(keys.modulus, BigUint::from(0xDEADBEEFu32));
        assert_eq!(keys.private_exp, BigUint::from(0xCAFEBABEu32));
        assert_eq!(keys.certificate, vec![0xFF, 0x00, 0xFF, 0x00]);
    }

    #[test]
    fn load_keys_too_few_lines() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "10001").unwrap();
        writeln!(tmp, "0102").unwrap();
        tmp.flush().unwrap();
        assert!(MtpzKeys::load(tmp.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn load_keys_missing_file() {
        assert!(MtpzKeys::load("/nonexistent/path/.mtpz-data").is_err());
    }

    #[test]
    fn sign_response_format() {
        let key = vec![0xABu8; 32];
        let sig = MtpzKeys::sign_response(&key).unwrap();
        assert_eq!(sig.len(), 20);
        assert_eq!(sig[0], 0x02);
        assert_eq!(sig[1], 0x03);
        assert_eq!(sig[2], 0x00);
        assert_eq!(sig[3], 0x10); // 16 bytes of CMAC follow
    }

    #[test]
    fn sign_response_matches_openssl() {
        // Verified against: echo -ne '\x00...\x01' | openssl mac -macopt hexkey:... CMAC
        let key = hex::decode(
            "5b77ddd5e97c73b5524874232e1919e109f873d31797a048e733d30bf6e0b6e3",
        )
        .unwrap();
        let sig = MtpzKeys::sign_response(&key).unwrap();
        // OpenSSL gives A12CFFDD28A2D859152C5736129B7FFC
        assert_eq!(hex::encode(&sig[4..]), "a12cffdd28a2d859152c5736129b7ffc");
    }

    #[test]
    fn sign_session_request_produces_4_values() {
        let key = vec![0xCDu8; 32];
        let cmac = MtpzKeys::sign_session_request(&key).unwrap();
        // Should produce 4 u32 values from 16-byte CMAC
        assert_eq!(cmac.len(), 4);
        // Values should not all be zero (CMAC of non-trivial input)
        assert!(cmac.iter().any(|&v| v != 0));
    }

    #[test]
    fn hkdf_deterministic() {
        let result1 = MtpzKeys::hkdf(b"test message", 32);
        let result2 = MtpzKeys::hkdf(b"test message", 32);
        assert_eq!(result1, result2);
        assert_eq!(result1.len(), 40); // ceil(32/20) * 20 = 40
    }

    #[test]
    fn hkdf_different_inputs() {
        let a = MtpzKeys::hkdf(b"aaa", 20);
        let b = MtpzKeys::hkdf(b"bbb", 20);
        assert_ne!(a, b);
    }

    #[test]
    fn aes_decrypt_roundtrip() {
        let key = [0x42u8; 16];
        let plaintext = [0u8; 16]; // One AES block
        // Encrypt: we don't have encrypt, but decrypt of zeros with zero IV is deterministic
        let result = MtpzKeys::aes_decrypt(&key, &plaintext).unwrap();
        assert_eq!(result.len(), 16);
    }

    #[test]
    fn aes_decrypt_wrong_key_size() {
        let result = MtpzKeys::aes_decrypt(&[0u8; 8], &[0u8; 16]);
        assert!(result.is_err());
    }

    #[test]
    fn cmac_rfc4493_test_vector() {
        // RFC 4493 Example 2: 16-byte message
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let msg = hex::decode("6bc1bee22e409f96e93d7e117393172a").unwrap();
        let result = MtpzKeys::cmac(&key, &msg).unwrap();
        assert_eq!(
            hex::encode(&result),
            "070a16b46b4d4144f79bdd9dd04a287c"
        );
    }
}
