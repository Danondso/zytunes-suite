//! Re-sign an iTunesDB in-place with hash58.
//! Usage: cargo run -p ipod-db --example sign_db --release -- <mount> <fwid>

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mount = std::path::PathBuf::from(&args[1]);
    let fwid_str = &args[2];
    let db_path = mount.join("iPod_Control/iTunes/iTunesDB");
    let mut data = std::fs::read(&db_path).unwrap();
    let fwid = ipod_db::hash::parse_firewire_id(fwid_str).unwrap();
    ipod_db::hash::sign_hash58(&mut data, &fwid).unwrap();
    std::fs::write(&db_path, &data).unwrap();
    println!("Signed {} bytes", data.len());
}
