//! Nix's base-32 encoding, bit-compatible with `nix hash convert`.

/// The Nix base-32 alphabet (no `e`, `o`, `t` or `u`).
pub const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Number of characters that encode `bytes` bytes.
pub const fn encoded_len(bytes: usize) -> usize {
    (bytes * 8).div_ceil(5)
}

/// Returns the value of one alphabet character.
pub fn digit(c: u8) -> Option<u8> {
    ALPHABET.iter().position(|&a| a == c).map(|p| p as u8)
}

/// Encodes bytes the way Nix does: the last character holds bits 0..5.
pub fn encode(bytes: &[u8]) -> String {
    let len = encoded_len(bytes.len());
    (0..len)
        .rev()
        .map(|n| ALPHABET[group(bytes, n * 5) as usize] as char)
        .collect()
}

fn group(bytes: &[u8], bit: usize) -> u8 {
    let (i, j) = (bit / 8, bit % 8);
    let low = u16::from(bytes[i]) >> j;
    let high = bytes.get(i + 1).map_or(0, |&b| u16::from(b) << (8 - j));
    ((low | high) & 0x1f) as u8
}

/// Decodes into exactly `N` bytes; `None` for a wrong length, a character
/// outside the alphabet, or set bits beyond the last byte (non-canonical).
pub fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != encoded_len(N) {
        return None;
    }
    let mut out = [0u8; N];
    for (n, c) in text.bytes().rev().enumerate() {
        place(&mut out, n * 5, digit(c)?)?;
    }
    Some(out)
}

fn place<const N: usize>(out: &mut [u8; N], bit: usize, value: u8) -> Option<()> {
    let (i, j) = (bit / 8, bit % 8);
    let shifted = u16::from(value) << j;
    out[i] |= shifted as u8;
    let carry = (shifted >> 8) as u8;
    match out.get_mut(i + 1) {
        Some(next) => *next |= carry,
        None if carry != 0 => return None,
        None => {}
    }
    Some(())
}
