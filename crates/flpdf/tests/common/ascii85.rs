//! Test-only ASCII85 fixture builder for integration tests.
//!
//! This deliberately stays out of production code paths: it exists only so
//! tests can construct fixed ASCII85-wrapped fixture bytes after the
//! production encoder route was removed.

pub fn fixture_bytes(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 5 / 4 + 2);

    let mut chunks = input.chunks_exact(4);
    for chunk in chunks.by_ref() {
        let value = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if value == 0 {
            out.push(b'z');
        } else {
            out.extend_from_slice(&ascii85_group(value));
        }
    }

    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        let mut padded = [0u8; 4];
        padded[..remainder.len()].copy_from_slice(remainder);
        let group = ascii85_group(u32::from_be_bytes(padded));
        out.extend_from_slice(&group[..remainder.len() + 1]);
    }

    out.extend_from_slice(b"~>");
    out
}

fn ascii85_group(value: u32) -> [u8; 5] {
    let mut v = value;
    let mut digits = [0u8; 5];
    for i in (0..5).rev() {
        digits[i] = (v % 85) as u8;
        v /= 85;
    }

    let mut chars = [0u8; 5];
    for (i, &digit) in digits.iter().enumerate() {
        chars[i] = digit + b'!';
    }
    chars
}
