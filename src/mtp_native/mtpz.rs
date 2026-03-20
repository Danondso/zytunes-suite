use crate::mtp::container::OperationCode;
use crate::mtp::session::MtpSession;

use aes::Aes128;
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
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
    pub fn load(path: &str) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Could not read {path}: {e}"))?;
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() < 5 {
            return Err(format!("Expected 5 lines in .mtpz-data, got {}", lines.len()));
        }

        let public_exp = BigUint::parse_bytes(lines[0].trim().as_bytes(), 16)
            .ok_or("Invalid public exponent")?;
        let session_key = hex::decode(lines[1].trim())
            .map_err(|e| format!("Invalid session key hex: {e}"))?;
        let modulus = BigUint::parse_bytes(lines[2].trim().as_bytes(), 16)
            .ok_or("Invalid modulus")?;
        let private_exp = BigUint::parse_bytes(lines[3].trim().as_bytes(), 16)
            .ok_or("Invalid private exponent")?;
        let certificate = hex::decode(lines[4].trim())
            .map_err(|e| format!("Invalid certificate hex: {e}"))?;

        Ok(MtpzKeys {
            public_exp,
            modulus,
            private_exp,
            session_key,
            certificate,
        })
    }

    /// Load from the default location ~/.mtpz-data.
    pub fn load_default() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME not set")?;
        Self::load(&format!("{home}/.mtpz-data"))
    }

    /// RSA size in bytes (128 for 1024-bit keys).
    fn rsa_size(&self) -> usize {
        (self.modulus.bits() as usize + 7) / 8
    }

    /// Raw RSA operation: data^exp mod n, with result zero-padded to rsa_size.
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

    /// HKDF-like key derivation using SHA-1 counter mode.
    /// Matches the C++ HKDF implementation in TrustedApp.cpp.
    fn hkdf(message: &[u8], key_size: usize) -> Vec<u8> {
        let block_size = 20; // SHA1 digest length
        let blocks = (key_size + block_size - 1) / block_size;
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

    /// Generate the MTPZ certificate message.
    /// Returns (challenge, message) matching GenerateCertificateMessage in the C++ code.
    fn generate_certificate_message(&self) -> (Vec<u8>, Vec<u8>) {
        let rsa_size = self.rsa_size();
        let message_size = 156 + self.certificate.len();

        // 16-byte random challenge.
        let mut challenge = vec![0u8; 16];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut challenge);

        let mut message = Vec::with_capacity(message_size);

        // Header: marker + flags + cert length.
        message.push(0x02);
        message.push(0x01);
        message.push(0x01);
        message.push(0x00);
        message.push(0x00);
        message.push((self.certificate.len() >> 8) as u8);
        message.push(self.certificate.len() as u8);

        // Certificate data.
        message.extend_from_slice(&self.certificate);

        // Challenge length + challenge.
        message.push((challenge.len() >> 8) as u8);
        message.push(challenge.len() as u8);
        message.extend_from_slice(&challenge);

        // Compute hash: SHA1(zeros(8) || SHA1(message[2..]))
        let inner_hash = Sha1::digest(&message[2..]);
        let mut salt = vec![0u8; 20 + 8];
        salt[8..].copy_from_slice(&inner_hash);
        let hash: Vec<u8> = Sha1::digest(&salt).to_vec();

        // Build PSS-like signature padding.
        let key = Self::hkdf(&hash, 107);

        let mut signature = vec![0u8; rsa_size];
        signature[106] = 1;
        for i in 0..hash.len() {
            signature[i + 107] = hash[i];
        }
        for i in 0..107 {
            signature[i] ^= key[i];
        }
        signature[0] &= 127;
        signature[rsa_size - 1] = 188; // 0xBC trailer

        // RSA sign (raw private key operation).
        let signed = self.rsa_raw(&signature);

        // Append signature header + signed data.
        message.push(0x01);
        message.push(0x00);
        message.push(signed.len() as u8);
        message.extend_from_slice(&signed);

        (challenge, message)
    }

    /// AES-128-CBC decrypt with zero IV.
    fn aes_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
        if key.len() != 16 {
            return Err(format!("AES key must be 16 bytes, got {}", key.len()));
        }
        let iv = [0u8; 16];
        let mut buf = data.to_vec();
        Aes128CbcDec::new(key.into(), &iv.into())
            .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
            .map_err(|e| format!("AES decrypt failed: {e}"))?;
        Ok(buf)
    }

    /// AES-128-CMAC.
    fn cmac(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
        let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key)
            .map_err(|e| format!("CMAC init: {e}"))?;
        mac.update(data);
        Ok(mac.finalize().into_bytes().to_vec())
    }

    /// Verify the device response and extract the CMAC key.
    /// Matches VerifyResponse in the C++ code.
    fn verify_response(
        &self,
        response: &[u8],
        original_challenge: &[u8],
    ) -> Result<Vec<u8>, String> {
        let rsa_size = self.rsa_size();
        let mut pos = 0;

        // Check marker.
        if response.len() < 4 {
            return Err("Response too short".to_string());
        }
        if response[pos] != 0x02 || response[pos + 1] != 0x02 {
            return Err(format!(
                "Invalid response tag: {:02x} {:02x}",
                response[pos],
                response[pos + 1]
            ));
        }
        pos += 2;

        let sig_size = ((response[pos] as usize) << 8) | (response[pos + 1] as usize);
        pos += 2;
        if sig_size < 0x80 || sig_size != rsa_size {
            return Err(format!("Invalid signature size: {sig_size}"));
        }

        // RSA decrypt the signature.
        let sig_data = &response[pos..pos + sig_size];
        let mut signature = self.rsa_raw(sig_data);
        pos += sig_size;

        // Unmask the signature (reverse PSS-like padding).
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

        // Extract AES key from signature bytes [0x70..0x80].
        let aes_key: Vec<u8> = signature[0x70..].to_vec();

        // Read encrypted payload.
        if pos + 4 > response.len() {
            return Err("Response too short for payload header".to_string());
        }
        if response[pos] != 0 || response[pos + 1] != 0 {
            return Err("Invalid payload record".to_string());
        }
        pos += 2;
        let payload_size = ((response[pos] as usize) << 8) | (response[pos + 1] as usize);
        pos += 2;

        if pos + payload_size > response.len() {
            return Err("Response too short for payload".to_string());
        }

        // Decrypt payload.
        let payload = Self::aes_decrypt(&aes_key, &response[pos..pos + payload_size])?;

        // Parse decrypted payload.
        let mut pp = 0;
        if payload.is_empty() || payload[pp] != 1 {
            return Err("Decryption failed (bad payload marker)".to_string());
        }
        pp += 1;

        // Certificate size (4 bytes big-endian).
        if pp + 4 > payload.len() {
            return Err("Payload too short for cert size".to_string());
        }
        let cert_size = ((payload[pp] as usize) << 24)
            | ((payload[pp + 1] as usize) << 16)
            | ((payload[pp + 2] as usize) << 8)
            | (payload[pp + 3] as usize);
        pp += 4;
        pp += cert_size; // Skip device certificates.

        // Verify our challenge is echoed back.
        if pp + 2 > payload.len() {
            return Err("Payload too short for challenge size".to_string());
        }
        let challenge_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        if challenge_size != original_challenge.len() {
            return Err(format!("Challenge size mismatch: {challenge_size}"));
        }
        if pp + challenge_size > payload.len() {
            return Err("Payload too short for challenge".to_string());
        }
        if &payload[pp..pp + challenge_size] != original_challenge {
            return Err("Challenge does not match!".to_string());
        }
        pp += challenge_size;

        // Skip device challenge.
        if pp + 2 > payload.len() {
            return Err("Payload too short for device challenge".to_string());
        }
        let dev_challenge_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        pp += dev_challenge_size;

        // Skip device signature.
        if pp + 3 > payload.len() {
            return Err("Payload too short for signature header".to_string());
        }
        if payload[pp] != 1 {
            return Err("Invalid signature marker".to_string());
        }
        pp += 1;
        let dev_sig_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        pp += dev_sig_size;

        // Read CMAC key.
        if pp + 3 > payload.len() {
            return Err("Payload too short for CMAC header".to_string());
        }
        if payload[pp] != 1 {
            return Err("Invalid CMAC record marker".to_string());
        }
        pp += 1;
        let cmac_size = ((payload[pp] as usize) << 8) | (payload[pp + 1] as usize);
        pp += 2;
        if pp + cmac_size > payload.len() {
            return Err("Payload too short for CMAC key".to_string());
        }

        Ok(payload[pp..pp + cmac_size].to_vec())
    }

    /// Build the confirmation message (SignResponse in C++).
    fn sign_response(cmac_key: &[u8]) -> Result<Vec<u8>, String> {
        let mut text = vec![0u8; 16];
        text[15] = 1;

        let mac = Self::cmac(&cmac_key[..16], &text)?;

        let mut message = Vec::with_capacity(20);
        message.push(0x02);
        message.push(0x03);
        message.push(0x00);
        message.push(0x10); // 16 bytes
        message.extend_from_slice(&mac);
        Ok(message)
    }

    /// Derive the session enabler CMAC values (SignSessionRequest in C++).
    fn sign_session_request(cmac_key: &[u8]) -> Result<[u32; 4], String> {
        // CMAC of 4 bytes starting at cmac_key[16..20] using first 16 bytes as key.
        let key = &cmac_key[..16];
        let data = if cmac_key.len() >= 20 {
            &cmac_key[16..20]
        } else {
            // Pad with zeros if key is shorter.
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
pub fn authenticate(session: &mut MtpSession, keys: &MtpzKeys) -> Result<(), String> {
    println!("  Starting MTPZ handshake...");

    // Step 1: End any existing trusted app session.
    let _ = session.execute_simple(OperationCode::EndTrustedAppSession, &[]);

    // Step 2: Generate and send certificate message.
    let (challenge, cert_message) = keys.generate_certificate_message();
    println!("  Generated certificate message ({} bytes)", cert_message.len());

    session.generic_operation_send(OperationCode::SendWMDRMPDAppRequest, &cert_message)?;
    println!("  Sent certificate to device");

    // Step 3: Receive and verify device response.
    let response = session.generic_operation_receive(OperationCode::GetWMDRMPDAppResponse)?;
    println!("  Received device response ({} bytes)", response.len());

    let cmac_key = keys.verify_response(&response, &challenge)?;
    println!("  Device response verified - challenge matched!");

    // Step 4: Send confirmation.
    let confirmation = MtpzKeys::sign_response(&cmac_key)?;
    session.generic_operation_send(OperationCode::SendWMDRMPDAppRequest, &confirmation)?;
    println!("  Sent confirmation to device");

    // Step 5: Enable secure file operations.
    let cmac_values = MtpzKeys::sign_session_request(&cmac_key)?;
    session.enable_secure_file_operations(cmac_values)?;
    println!("  Secure file operations enabled!");

    Ok(())
}
