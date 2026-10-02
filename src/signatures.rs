//! Lossless prefilter: collisions admit extra candidates, final verifier decides.
#[derive(Default, Clone)]
pub struct ShortSignature {
    pub bytes: [u64; 4],
    pub pairs: u128,
}
fn bits(key: u32) -> u128 {
    // Integer mixing, two positions. No third-party hashing implementation.
    let mut mixed = u64::from(key).wrapping_mul(0x9e3779b97f4a7c15);
    mixed ^= mixed >> 29;
    mixed = mixed.wrapping_mul(0xd6e8feb86659fd93);
    mixed ^= mixed >> 32;
    (1u128 << ((mixed >> 7) & 127)) | (1u128 << ((mixed >> 39) & 127))
}
pub(super) fn trigram_signature(bytes: &[u8]) -> u128 {
    bytes
        .windows(3)
        .fold(0, |signature, b| signature | bits(crate::index::gram(b)))
}
impl ShortSignature {
    pub fn insert(&mut self, data: &[u8]) {
        for b in data {
            self.bytes[*b as usize / 64] |= 1u64 << (*b as usize % 64);
        }
        for pair in data.windows(2) {
            self.pairs |= bits(u32::from(pair[0]) | u32::from(pair[1]) << 8);
        }
    }
    pub fn contains(&self, needle: &Self) -> bool {
        self.bytes
            .iter()
            .zip(needle.bytes)
            .all(|(hay, n)| hay & n == n)
            && self.pairs & needle.pairs == needle.pairs
    }
}
