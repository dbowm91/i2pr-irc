use i2pr_irc_wire::{LineDecoder, Message};
fn main() {
    let mut seed = 0x4d595df4d0f33173u64;
    for _ in 0..10000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let n = (seed as usize) % 8192;
        let mut bytes = Vec::with_capacity(n);
        for _ in 0..n {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            bytes.push(seed as u8)
        }
        let _ = Message::parse(&bytes);
        let mut decoder = LineDecoder::default();
        for chunk in bytes.chunks(31) {
            let _ = decoder.push(chunk);
        }
    }
}
