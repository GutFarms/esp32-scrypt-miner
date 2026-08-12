//! Litecoin-style scrypt proof-of-work miner.
//!
//! Algorithm: `scrypt(header, header, N=1024, r=1, p=1, dkLen=32)` then compare
//! the little-endian hash against a compact target.
//!
//! With the `lite` feature the ROMix V array is stored sparsely (time–memory
//! tradeoff) so classic ESP32 + WiFi can still produce **pool-valid** Litecoin
//! hashes without a 128 KiB scratchpad.

#![allow(clippy::needless_range_loop)]

use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// log2(N). Litecoin / Dogecoin use **10** (N=1024).
pub const SCRYPT_LOG_N: u8 = 10;
pub const SCRYPT_N: usize = 1 << SCRYPT_LOG_N;
pub const SCRYPT_R: usize = 1;
pub const SCRYPT_P: usize = 1;
pub const HASH_LEN: usize = 32;
pub const HEADER_LEN: usize = 80;

/// How many 128·r-byte blocks we keep for ROMix.
///
/// - full (`not lite`): all `N` blocks (128 KiB) — fastest
/// - `lite`: 64 checkpoints (8 KiB) — stride 16, still **N=1024** hashes
#[cfg(feature = "lite")]
pub const V_SLOTS: usize = 64;
#[cfg(not(feature = "lite"))]
pub const V_SLOTS: usize = SCRYPT_N;

/// Bytes for the ROMix V / checkpoint buffer: 128 * V_SLOTS * r
pub const V_BYTES: usize = 128 * V_SLOTS * SCRYPT_R;
/// Bytes for the XY scratch buffer: 256 * r
pub const XY_BYTES: usize = 256 * SCRYPT_R;

/// Snapshot of miner stats for the display / host UI.
#[derive(Clone, Debug, Default)]
pub struct MinerStats {
    pub nonce: u32,
    pub hashes: u64,
    pub shares: u64,
    pub hashrate_x100: u32,
    pub best_hash: [u8; HASH_LEN],
    pub last_share_nonce: Option<u32>,
}

/// Result of hashing a single nonce.
#[derive(Clone, Debug)]
pub struct HashResult {
    pub nonce: u32,
    pub hash: [u8; HASH_LEN],
    pub is_share: bool,
}

/// Scrypt PoW engine with reusable working buffers.
pub struct ScryptMiner {
    header: [u8; HEADER_LEN],
    /// Compact target: hash (as LE uint256) must be < target.
    target: [u8; HASH_LEN],
    nonce: u32,
    hashes: u64,
    shares: u64,
    best_hash: [u8; HASH_LEN],
    last_share_nonce: Option<u32>,
    /// ROMix V / checkpoint buffer
    v: alloc::vec::Vec<u8>,
    /// XY scratch
    xy: alloc::vec::Vec<u8>,
}

impl ScryptMiner {
    /// Create a miner with a demo header and difficulty (leading zero nibbles).
    ///
    /// `leading_zero_nibbles` ≈ difficulty knob for local demos (4–8 is typical).
    pub fn new_demo(leading_zero_nibbles: u8) -> Self {
        let mut header = [0u8; HEADER_LEN];
        // Version
        header[0..4].copy_from_slice(&1u32.to_le_bytes());
        // Fake prevhash / merkle / time / bits — deterministic demo work
        for (i, b) in header[4..76].iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17).wrapping_add(0xA5);
        }
        header[68..72].copy_from_slice(&1_700_000_000u32.to_le_bytes());
        header[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());

        let target = target_from_leading_zero_nibbles(leading_zero_nibbles);
        Self::with_job(header, target, 0)
    }

    /// Start mining a specific 80-byte header and 32-byte LE target.
    pub fn with_job(header: [u8; HEADER_LEN], target: [u8; HASH_LEN], start_nonce: u32) -> Self {
        let best_hash = [0xffu8; HASH_LEN];
        Self {
            header,
            target,
            nonce: start_nonce,
            hashes: 0,
            shares: 0,
            best_hash,
            last_share_nonce: None,
            v: alloc::vec![0u8; V_BYTES],
            xy: alloc::vec![0u8; XY_BYTES],
        }
    }

    pub fn stats(&self) -> MinerStats {
        MinerStats {
            nonce: self.nonce,
            hashes: self.hashes,
            shares: self.shares,
            hashrate_x100: 0,
            best_hash: self.best_hash,
            last_share_nonce: self.last_share_nonce,
        }
    }

    pub fn header(&self) -> &[u8; HEADER_LEN] {
        &self.header
    }

    pub fn target(&self) -> &[u8; HASH_LEN] {
        &self.target
    }

    /// Replace the active header/target while keeping ROMix buffers allocated.
    pub fn set_job(&mut self, header: [u8; HEADER_LEN], target: [u8; HASH_LEN], start_nonce: u32) {
        self.header = header;
        self.target = target;
        self.nonce = start_nonce;
        self.best_hash = [0xffu8; HASH_LEN];
        self.last_share_nonce = None;
    }

    /// Hash the current nonce, advance, and return the result.
    pub fn mine_one(&mut self) -> HashResult {
        self.header[76..80].copy_from_slice(&self.nonce.to_le_bytes());
        let hash = scrypt_hash(&self.header, &mut self.v, &mut self.xy);
        let is_share = hash_meets_target(&hash, &self.target);

        if compare_hash_le(&hash, &self.best_hash) == core::cmp::Ordering::Less {
            self.best_hash = hash;
        }

        let nonce = self.nonce;
        if is_share {
            self.shares = self.shares.saturating_add(1);
            self.last_share_nonce = Some(nonce);
        }

        self.hashes = self.hashes.saturating_add(1);
        self.nonce = self.nonce.wrapping_add(1);

        HashResult {
            nonce,
            hash,
            is_share,
        }
    }

    /// Mine up to `count` hashes; return the last result and whether any share was found.
    pub fn mine_batch(&mut self, count: usize) -> (HashResult, bool) {
        let mut any_share = false;
        let mut last = HashResult {
            nonce: self.nonce,
            hash: [0xff; HASH_LEN],
            is_share: false,
        };
        for _ in 0..count {
            last = self.mine_one();
            if last.is_share {
                any_share = true;
            }
        }
        (last, any_share)
    }
}

/// Build a target with `n` leading zero nibbles in the usual hex display.
///
/// Litecoin/Bitcoin print hashes by reversing the LE bytes, so leading display
/// zeros correspond to zeros at the **high** end of the LE byte array.
pub fn target_from_leading_zero_nibbles(n: u8) -> [u8; HASH_LEN] {
    let mut target = [0xffu8; HASH_LEN];
    let n = n.min(64);
    let full_bytes = (n / 2) as usize;
    let half = n % 2 == 1;
    for i in 0..full_bytes.min(HASH_LEN) {
        target[HASH_LEN - 1 - i] = 0x00;
    }
    if half && full_bytes < HASH_LEN {
        // One more leading zero nibble ⇒ high nibble of next MSB must be 0.
        target[HASH_LEN - 1 - full_bytes] = 0x0f;
    }
    // Ensure target is never all-zero (impossible share).
    if target.iter().all(|&b| b == 0) {
        target[0] = 0x01;
    }
    target
}

/// True if `hash` < `target` as unsigned little-endian 256-bit integers.
pub fn hash_meets_target(hash: &[u8; HASH_LEN], target: &[u8; HASH_LEN]) -> bool {
    compare_hash_le(hash, target) == core::cmp::Ordering::Less
}

fn compare_hash_le(a: &[u8; HASH_LEN], b: &[u8; HASH_LEN]) -> core::cmp::Ordering {
    for i in (0..HASH_LEN).rev() {
        match a[i].cmp(&b[i]) {
            core::cmp::Ordering::Equal => {}
            ord => return ord,
        }
    }
    core::cmp::Ordering::Equal
}

/// Compute Litecoin scrypt hash of an 80-byte header using caller-provided buffers.
pub fn scrypt_hash(header: &[u8; HEADER_LEN], v: &mut [u8], xy: &mut [u8]) -> [u8; HASH_LEN] {
    let mut out = [0u8; HASH_LEN];
    scrypt_general(
        header,
        header,
        SCRYPT_N,
        SCRYPT_R,
        SCRYPT_P,
        v,
        xy,
        &mut out,
    );
    out
}

/// General scrypt (RFC 7914) with caller-provided ROMix buffers.
///
/// When `v` holds fewer than `n` blocks, a checkpointed ROMix (TMTO) is used so
/// the digest still matches full-memory scrypt for the same `n`.
pub fn scrypt_general(
    password: &[u8],
    salt: &[u8],
    n: usize,
    r: usize,
    p: usize,
    v: &mut [u8],
    xy: &mut [u8],
    out: &mut [u8],
) {
    let block_bytes = 128 * r;
    assert!(n.is_power_of_two() && n >= 2);
    assert!(xy.len() >= 2 * block_bytes);
    let slots = v.len() / block_bytes;
    assert!(slots >= 2, "V buffer too small");
    assert!(
        slots >= n || n % slots == 0,
        "V slots must divide N for TMTO ROMix"
    );

    let mut b = alloc::vec![0u8; block_bytes * p];
    pbkdf2_sha256(password, salt, 1, &mut b);

    for i in 0..p {
        let start = i * block_bytes;
        let end = start + block_bytes;
        scrypt_romix(&mut b[start..end], n, r, v, xy);
    }

    pbkdf2_sha256(password, &b, 1, out);
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], rounds: u32, out: &mut [u8]) {
    pbkdf2::<HmacSha256>(password, salt, rounds, out).expect("HMAC-SHA256 PBKDF2");
}

/// scrypt ROMix (RFC 7914), with automatic full-memory or TMTO path.
fn scrypt_romix(b: &mut [u8], n: usize, r: usize, v: &mut [u8], xy: &mut [u8]) {
    let block_bytes = 128 * r;
    debug_assert_eq!(b.len(), block_bytes);
    debug_assert!(xy.len() >= 2 * block_bytes);

    let slots = v.len() / block_bytes;
    if slots >= n {
        scrypt_romix_full(b, n, r, v, xy);
    } else {
        let stride = n / slots;
        scrypt_romix_tmto(b, n, r, stride, v, xy);
    }
}

fn scrypt_romix_full(b: &mut [u8], n: usize, r: usize, v: &mut [u8], xy: &mut [u8]) {
    let block_bytes = 128 * r;
    let (x, y) = xy.split_at_mut(block_bytes);
    x[..block_bytes].copy_from_slice(b);

    for i in 0..n {
        v[i * block_bytes..(i + 1) * block_bytes].copy_from_slice(x);
        scrypt_block_mix(x, y, r);
        x.copy_from_slice(y);
    }

    for _ in 0..n {
        let j = integerify(x, r) % n;
        xor_block(x, &v[j * block_bytes..(j + 1) * block_bytes]);
        scrypt_block_mix(x, y, r);
        x.copy_from_slice(y);
    }

    b.copy_from_slice(x);
}

/// Checkpointed ROMix: store every `stride`-th `V[i]`, recompute intermediates.
///
/// Produces the same digest as full-memory ROMix for the same `n`.
fn scrypt_romix_tmto(
    b: &mut [u8],
    n: usize,
    r: usize,
    stride: usize,
    v: &mut [u8],
    xy: &mut [u8],
) {
    let block_bytes = 128 * r;
    assert_eq!(r, 1, "TMTO ROMix supports r=1 (Litecoin)");
    debug_assert!(stride >= 2 && n % stride == 0);
    debug_assert_eq!(v.len() / block_bytes, n / stride);

    let (x, y) = xy.split_at_mut(block_bytes);
    x[..block_bytes].copy_from_slice(b);

    for i in 0..n {
        if i % stride == 0 {
            let slot = i / stride;
            v[slot * block_bytes..(slot + 1) * block_bytes].copy_from_slice(x);
        }
        scrypt_block_mix(x, y, r);
        x.copy_from_slice(y);
    }

    // Scratch for reconstructed V[j] — stack is fine for r=1 (128 bytes).
    let mut t = [0u8; 128];
    let t = &mut t[..block_bytes];

    for _ in 0..n {
        let j = integerify(x, r) % n;
        recover_v_block(t, j, n, r, stride, v, y);
        xor_block(x, t);
        scrypt_block_mix(x, y, r);
        x.copy_from_slice(y);
    }

    b.copy_from_slice(x);
}

/// Reconstruct `V[j]` from the nearest stored checkpoint into `out`.
///
/// Uses `scratch` (one block) as BlockMix output.
fn recover_v_block(
    out: &mut [u8],
    j: usize,
    _n: usize,
    r: usize,
    stride: usize,
    v: &[u8],
    scratch: &mut [u8],
) {
    let block_bytes = 128 * r;
    let base = (j / stride) * stride;
    let slot = base / stride;
    out.copy_from_slice(&v[slot * block_bytes..(slot + 1) * block_bytes]);
    for _ in base..j {
        scrypt_block_mix(out, scratch, r);
        out.copy_from_slice(&scratch[..block_bytes]);
    }
}

/// Integerify: interpret last 64 bits of block as LE u64.
fn integerify(b: &[u8], r: usize) -> usize {
    let offset = (2 * r - 1) * 64;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&b[offset..offset + 8]);
    u64::from_le_bytes(bytes) as usize
}

fn xor_block(a: &mut [u8], b: &[u8]) {
    for (x, y) in a.iter_mut().zip(b.iter()) {
        *x ^= *y;
    }
}

/// scrypt BlockMix using Salsa20/8.
fn scrypt_block_mix(b: &[u8], y: &mut [u8], r: usize) {
    let mut x = [0u8; 64];
    x.copy_from_slice(&b[(2 * r - 1) * 64..2 * r * 64]);

    for i in 0..2 * r {
        let bi = &b[i * 64..(i + 1) * 64];
        for (xi, &bi_byte) in x.iter_mut().zip(bi.iter()) {
            *xi ^= bi_byte;
        }
        salsa20_8(&mut x);
        let dest = if i % 2 == 0 {
            (i / 2) * 64
        } else {
            (r + (i - 1) / 2) * 64
        };
        y[dest..dest + 64].copy_from_slice(&x);
    }
}

#[inline(always)]
fn quarterround(a: &mut u32, b: &mut u32, c: &mut u32, d: &mut u32) {
    *b ^= a.wrapping_add(*d).rotate_left(7);
    *c ^= b.wrapping_add(*a).rotate_left(9);
    *d ^= c.wrapping_add(*b).rotate_left(13);
    *a ^= d.wrapping_add(*c).rotate_left(18);
}

/// Salsa20/8 core (8 rounds = 4 double-rounds) on a 64-byte block (RFC 7914).
fn salsa20_8(block: &mut [u8; 64]) {
    let mut x = [0u32; 16];
    for i in 0..16 {
        x[i] = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    let orig = x;

    for _ in 0..4 {
        // column rounds
        let [ref mut x0, ref mut x1, ref mut x2, ref mut x3, ref mut x4, ref mut x5, ref mut x6, ref mut x7, ref mut x8, ref mut x9, ref mut x10, ref mut x11, ref mut x12, ref mut x13, ref mut x14, ref mut x15] =
            x;
        quarterround(x0, x4, x8, x12);
        quarterround(x5, x9, x13, x1);
        quarterround(x10, x14, x2, x6);
        quarterround(x15, x3, x7, x11);
        // row rounds
        quarterround(x0, x1, x2, x3);
        quarterround(x5, x6, x7, x4);
        quarterround(x10, x11, x8, x9);
        quarterround(x15, x12, x13, x14);
    }

    for i in 0..16 {
        let val = x[i].wrapping_add(orig[i]);
        block[i * 4..i * 4 + 4].copy_from_slice(&val.to_le_bytes());
    }
}

/// Format first `n` bytes of a hash as lowercase hex into `buf`.
pub fn hash_to_hex(hash: &[u8], n: usize, buf: &mut heapless::String<128>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    buf.clear();
    for &byte in hash.iter().take(n) {
        let _ = buf.push(HEX[(byte >> 4) as usize] as char);
        let _ = buf.push(HEX[(byte & 0xf) as usize] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_to_bytes<const N: usize>(hex: &str) -> [u8; N] {
        let hex = hex.trim();
        assert_eq!(hex.len(), N * 2);
        let mut out = [0u8; N];
        for i in 0..N {
            out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn salsa20_8_known_vector() {
        // RFC 7914 / scrypt salsa20/8 test vector input
        let mut block = [
            0x7e, 0x87, 0x9a, 0x21, 0x4f, 0x3e, 0xc9, 0x86, 0x7c, 0xa9, 0x40, 0xe6, 0x41, 0x71,
            0x8f, 0x26, 0xba, 0xee, 0x55, 0x5b, 0x8c, 0x61, 0xc1, 0xb5, 0x0d, 0xf8, 0x46, 0x11,
            0x6d, 0xcd, 0x3b, 0x1d, 0xee, 0x24, 0xf3, 0x19, 0xdf, 0x9b, 0x3d, 0x85, 0x14, 0x12,
            0x1e, 0x4b, 0x5a, 0xc5, 0xaa, 0x32, 0x76, 0x02, 0x1d, 0x29, 0x09, 0xc7, 0x48, 0x29,
            0xed, 0xeb, 0xc6, 0x8d, 0xb8, 0xb8, 0xc2, 0x5e,
        ];
        // RFC 7914 §8
        let expected = [
            0xa4, 0x1f, 0x85, 0x9c, 0x66, 0x08, 0xcc, 0x99, 0x3b, 0x81, 0xca, 0xcb, 0x02, 0x0c,
            0xef, 0x05, 0x04, 0x4b, 0x21, 0x81, 0xa2, 0xfd, 0x33, 0x7d, 0xfd, 0x7b, 0x1c, 0x63,
            0x96, 0x68, 0x2f, 0x29, 0xb4, 0x39, 0x31, 0x68, 0xe3, 0xc9, 0xe6, 0xbc, 0xfe, 0x6b,
            0xc5, 0xb7, 0xa0, 0x6d, 0x96, 0xba, 0xe4, 0x24, 0xcc, 0x10, 0x2c, 0x91, 0x74, 0x5c,
            0x24, 0xad, 0x67, 0x3d, 0xc7, 0x61, 0x8f, 0x81,
        ];
        salsa20_8(&mut block);
        assert_eq!(block, expected);
    }

    #[test]
    fn target_comparison_works() {
        let easy = target_from_leading_zero_nibbles(2); // MSB byte must be 0
        let mut hash = [0xff; HASH_LEN];
        hash[HASH_LEN - 1] = 0x00;
        hash[HASH_LEN - 2] = 0x10;
        assert!(hash_meets_target(&hash, &easy));

        hash[HASH_LEN - 1] = 0x10;
        assert!(!hash_meets_target(&hash, &easy));
    }

    #[test]
    fn miner_finds_easy_share() {
        // Very easy target so a share appears quickly even with full Litecoin N.
        let mut miner = ScryptMiner::new_demo(2);
        let mut found = false;
        for _ in 0..512 {
            if miner.mine_one().is_share {
                found = true;
                break;
            }
        }
        assert!(found, "expected a share within 512 hashes at easy difficulty");
        assert!(miner.stats().shares >= 1);
    }

    #[test]
    fn scrypt_is_deterministic() {
        let header = [0x42u8; HEADER_LEN];
        let mut v1 = alloc::vec![0u8; V_BYTES];
        let mut xy1 = alloc::vec![0u8; XY_BYTES];
        let mut v2 = alloc::vec![0u8; V_BYTES];
        let mut xy2 = alloc::vec![0u8; XY_BYTES];
        let h1 = scrypt_hash(&header, &mut v1, &mut xy1);
        let h2 = scrypt_hash(&header, &mut v2, &mut xy2);
        assert_eq!(h1, h2);
    }

    #[test]
    fn rfc7914_scrypt_vector() {
        // RFC 7914 §12: P="", S="", N=16, r=1, p=1, dkLen=64
        let n = 16usize;
        let r = 1usize;
        let mut v = alloc::vec![0u8; 128 * n * r];
        let mut xy = alloc::vec![0u8; 256 * r];
        let mut out = [0u8; 64];
        scrypt_general(b"", b"", n, r, 1, &mut v, &mut xy, &mut out);
        let expected: [u8; 64] = [
            0x77, 0xd6, 0x57, 0x62, 0x38, 0x65, 0x7b, 0x20, 0x3b, 0x19, 0xca, 0x42, 0xc1, 0x8a,
            0x04, 0x97, 0xf1, 0x6b, 0x48, 0x44, 0xe3, 0x07, 0x4a, 0xe8, 0xdf, 0xdf, 0xfa, 0x3f,
            0xed, 0xe2, 0x14, 0x42, 0xfc, 0xd0, 0x06, 0x9d, 0xed, 0x09, 0x48, 0xf8, 0x32, 0x6a,
            0x75, 0x3a, 0x0f, 0xc8, 0x1f, 0x17, 0xe8, 0xd3, 0xe0, 0xfb, 0x2e, 0x0d, 0x36, 0x28,
            0xcf, 0x35, 0xe2, 0x0c, 0x38, 0xd1, 0x89, 0x06,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn tmto_romix_matches_full_memory() {
        // N=64 with only 8 slots (stride 8) must match a full 64-slot buffer.
        let n = 64usize;
        let r = 1usize;
        let password = b"tmto-check-password!!";
        let salt = password;
        let mut full_v = alloc::vec![0u8; 128 * n * r];
        let mut tmto_v = alloc::vec![0u8; 128 * (n / 8) * r];
        let mut xy = alloc::vec![0u8; 256 * r];
        let mut out_full = [0u8; 32];
        let mut out_tmto = [0u8; 32];
        scrypt_general(password, salt, n, r, 1, &mut full_v, &mut xy, &mut out_full);
        scrypt_general(password, salt, n, r, 1, &mut tmto_v, &mut xy, &mut out_tmto);
        assert_eq!(out_full, out_tmto);
    }

    #[test]
    fn litecoin_block_29255_pow_hash() {
        // Litecoin wiki block #29255 header + scrypt PoW hash (BE display).
        let header = hex_to_bytes::<80>(
            "01000000f615f7ce3b4fc6b8f61e8f89aedb1d0852507650533a9e3b10b9bbcc30639f279fcaa86746e1ef52d3edb3c4ad8259920d509bd073605c9bf1d59983752a6b06b817bb4ea78e011d012d59d4",
        );
        let expected_be = hex_to_bytes::<32>(
            "0000000110c8357966576df46f3b802ca897deb7ad18b12f1c24ecff6386ebd9",
        );
        let mut expected_le = expected_be;
        expected_le.reverse();

        let mut v = alloc::vec![0u8; V_BYTES];
        let mut xy = alloc::vec![0u8; XY_BYTES];
        let hash = scrypt_hash(&header, &mut v, &mut xy);
        assert_eq!(hash, expected_le, "must match Litecoin scrypt N=1024 PoW");
    }
}
