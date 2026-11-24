//! The hash that locks a font file (SPEC §2.4).

use std::fmt::{self, Debug, Display, Formatter};

use sha2::{Digest, Sha256};

/// The SHA-256 hash of a font file, which `folio.lock` locks a face by and
/// the font cache stores the file under. It prints as `folio.lock` writes
/// it: `sha256:` and 64 lower-case hex digits.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FontHash(pub [u8; 32]);

impl FontHash {
    /// The hash of a file's contents.
    pub fn of(data: &[u8]) -> Self {
        Self(Sha256::digest(data).into())
    }

    /// The 64 lower-case hex digits, without the `sha256:` prefix.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Parses 64 lower-case hex digits, as [`Self::hex`] writes them.
    pub fn from_hex(hex: &str) -> Option<Self> {
        if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut out = [0; 32];
        for (byte, pair) in out.iter_mut().zip(hex.as_bytes().chunks(2)) {
            let digit = |b: u8| {
                if b.is_ascii_digit() {
                    b - b'0'
                } else {
                    b - b'a' + 10
                }
            };
            *byte = digit(pair[0]) << 4 | digit(pair[1]);
        }
        Some(Self(out))
    }
}

impl Display for FontHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl Debug for FontHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_round_trips() {
        let hash = FontHash::of(b"abc");
        assert_eq!(
            hash.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(FontHash::from_hex(&hash.hex()), Some(hash));
        assert_eq!(FontHash::from_hex("BA78"), None);
    }
}
