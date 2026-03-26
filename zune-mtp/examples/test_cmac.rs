// RFC 4493 AES-CMAC test vectors
fn main() {
    use aes::Aes128;
    use cmac::{Cmac, Mac};

    // Test vector from RFC 4493, Example 2 (16-byte message)
    let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
    let msg = hex::decode("6bc1bee22e409f96e93d7e117393172a").unwrap();

    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(&key).unwrap();
    mac.update(&msg);
    let result = mac.finalize().into_bytes();
    let hex_result = hex::encode(result);

    println!("CMAC result: {}", hex_result);
    println!("Expected:    070a16b46b4d4144f79bdd9dd04a287c");
    println!("Match: {}", hex_result == "070a16b46b4d4144f79bdd9dd04a287c");

    // Also test with the same pattern as SignResponse: CMAC of [0..0, 1] with arbitrary key
    let test_key = hex::decode("d2c4208cbb038aa4081644fcf21ba915").unwrap();
    let mut text = vec![0u8; 16];
    text[15] = 1;
    let mut mac2 = <Cmac<Aes128> as Mac>::new_from_slice(&test_key).unwrap();
    mac2.update(&text);
    let result2 = mac2.finalize().into_bytes();
    println!("\nSignResponse-style CMAC: {}", hex::encode(result2));
}
