//! Lowercase hex for digests: sha2 0.11's output type has no `{:x}` formatting.

pub(crate) fn lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn sha256_of_nothing_is_the_well_known_value() {
        assert_eq!(
            lower(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn every_byte_is_two_digits() {
        assert_eq!(lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(lower(&[]), "");
    }
}
