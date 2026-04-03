use aes::Aes128;
use cmac::{Cmac, Mac};

fn main() {
    // Known-answer CMAC vector
    let cmac_key =
        hex::decode("5b77ddd5e97c73b5524874232e1919e109f873d31797a048e733d30bf6e0b6e3").unwrap();

    // Our SignResponse implementation
    let key = &cmac_key[..16];
    let mut text = vec![0u8; 16];
    text[15] = 1;

    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key).unwrap();
    mac.update(&text);
    let result = mac.finalize().into_bytes();

    let mut message = Vec::with_capacity(20);
    message.push(0x02);
    message.push(0x03);
    message.push(0x00);
    message.push(0x10);
    message.extend_from_slice(&result);

    println!("Our signature:  {}", hex::encode(&message));
    println!("Expected:       02030010a12cffdd28a2d815152c5736129b7ffc");
    println!(
        "Match: {}",
        hex::encode(&message) == "02030010a12cffdd28a2d815152c5736129b7ffc"
    );
}
