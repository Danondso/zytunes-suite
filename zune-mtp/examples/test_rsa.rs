use num_bigint::BigUint;

fn main() {
    // Load keys from .mtpz-data
    let home = std::env::var("HOME").unwrap();
    let content = std::fs::read_to_string(format!("{home}/.mtpz-data")).unwrap();
    let lines: Vec<&str> = content.lines().collect();

    let public_exp = BigUint::parse_bytes(lines[0].trim().as_bytes(), 16).unwrap();
    let modulus = BigUint::parse_bytes(lines[2].trim().as_bytes(), 16).unwrap();
    let private_exp = BigUint::parse_bytes(lines[3].trim().as_bytes(), 16).unwrap();

    // Test: encrypt a known value with the public key, decrypt with private key
    let test_data = vec![0x42u8; 128]; // 128 bytes of 0x42
    let m = BigUint::from_bytes_be(&test_data);

    // "Encrypt" with private key (signing): m^d mod n
    let signed = m.modpow(&private_exp, &modulus);
    let signed_bytes = {
        let raw = signed.to_bytes_be();
        let mut padded = vec![0u8; 128];
        let start = 128usize.saturating_sub(raw.len());
        padded[start..].copy_from_slice(&raw[..128.min(raw.len())]);
        padded
    };

    // "Decrypt" with public key (verify): s^e mod n
    let verified = BigUint::from_bytes_be(&signed_bytes).modpow(&public_exp, &modulus);
    let verified_bytes = {
        let raw = verified.to_bytes_be();
        let mut padded = vec![0u8; 128];
        let start = 128usize.saturating_sub(raw.len());
        padded[start..].copy_from_slice(&raw[..128.min(raw.len())]);
        padded
    };

    println!("Input:    {}", hex::encode(&test_data));
    println!("Signed:   {}", hex::encode(&signed_bytes));
    println!("Verified: {}", hex::encode(&verified_bytes));
    println!("Roundtrip match: {}", test_data == verified_bytes);

    // Also test with OpenSSL if available
    let n_hex = lines[2].trim();
    let d_hex = lines[3].trim();
    println!("\nModulus (first 20 chars): {}...", &n_hex[..20]);
    println!("Private exp (first 20 chars): {}...", &d_hex[..20]);
    println!("Public exp: {}", lines[0].trim());
}
