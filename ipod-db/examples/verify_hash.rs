//! Verify our hash58 implementation against a real iTunesDB.
//!
//! Usage:
//!   cargo run -p ipod-db --example verify_hash -- /mnt/ipod-classic 000A2700215CDB22

use ipod_db::hash;

fn main() {
    let mount = std::env::args()
        .nth(1)
        .expect("usage: verify_hash <mount> <firewire_id>");
    let fwid = std::env::args()
        .nth(2)
        .expect("usage: verify_hash <mount> <firewire_id>");

    let db_path = format!("{mount}/iPod_Control/iTunes/iTunesDB");
    let mut data = std::fs::read(&db_path).unwrap();

    // Save the original hash.
    let original_hash: Vec<u8> = data[0x58..0x6C].to_vec();
    println!("Original hash58: {}", hex(&original_hash));

    // Compute our hash.
    let firewire_id = hash::parse_firewire_id(&fwid).unwrap();
    hash::sign_hash58(&mut data, &firewire_id).unwrap();

    let our_hash: Vec<u8> = data[0x58..0x6C].to_vec();
    println!("Our hash58:      {}", hex(&our_hash));

    if original_hash == our_hash {
        println!("\nMATCH! Our hash58 implementation is correct.");
    } else {
        println!("\nMISMATCH. Algorithm or key derivation is wrong.");
        // Show intermediate values for debugging.
        println!("FirewireGuid bytes: {:02x?}", &firewire_id[..8]);
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
