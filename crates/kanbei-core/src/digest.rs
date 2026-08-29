//! Content digests: blake3 over canonical bytes, text form `blake3:<64 hex>`.
//! This exact string is what objects and refs carry (S6 fixture used
//! `blake3:...` strings; here refs are typed [`Digest`]s).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

pub const ALG: &str = "blake3";

const HEX_LEN: usize = 64;
const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    pub fn new(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 64 lowercase hex chars, no prefix.
    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(HEX_LEN);
        for b in self.0 {
            s.push(HEX_CHARS[(b >> 4) as usize] as char);
            s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// Parses the full text form `blake3:<64 hex>` (same as [`FromStr`]).
    pub fn from_hex(s: &str) -> Result<Self, DigestParseError> {
        s.parse()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(ALG)?;
        f.write_str(":")?;
        for b in self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

fn hex_val(c: u8, at: usize) -> Result<u8, DigestParseError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(DigestParseError::InvalidHex(c as char, at)),
    }
}

impl FromStr for Digest {
    type Err = DigestParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // canonical form is lowercase-only; the text form is frozen at M1
        let hex = s
            .strip_prefix("blake3:")
            .ok_or_else(|| DigestParseError::BadPrefix(s.to_string()))?;
        if hex.len() != HEX_LEN {
            return Err(DigestParseError::WrongLen(hex.len()));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            out[i] = (hex_val(chunk[0], i * 2)? << 4) | hex_val(chunk[1], i * 2 + 1)?;
        }
        Ok(Digest(out))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DigestParseError {
    #[error("digest must start with {ALG}:, got {0:?}")]
    BadPrefix(String),
    #[error("invalid hex character {0:?} at {1}")]
    InvalidHex(char, usize),
    #[error("digest hex has {0} chars, expected {HEX_LEN}")]
    WrongLen(usize),
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // blake3 of the empty input
    const EMPTY: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn roundtrip() {
        let d = Digest::new(b"hello world");
        let s = d.to_string();
        assert_eq!(s.parse::<Digest>().unwrap(), d);
        assert_eq!(Digest::from_hex(&s).unwrap(), d);
    }

    #[test]
    fn known_hash() {
        assert_eq!(Digest::new(b"").hex(), EMPTY);
        assert_eq!(Digest::new(b"").to_string(), format!("blake3:{EMPTY}"));
    }

    #[test]
    fn hex_is_lowercase() {
        let d = Digest::new(b"abc");
        assert_eq!(d.hex().len(), 64);
        assert_eq!(d.hex(), d.hex().to_lowercase());
        assert!(d.hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn rejects_bad_prefix() {
        assert!(matches!(
            "sha256:af13".parse::<Digest>(),
            Err(DigestParseError::BadPrefix(_))
        ));
        assert!(matches!(
            "".parse::<Digest>(),
            Err(DigestParseError::BadPrefix(_))
        ));
    }

    #[test]
    fn rejects_bad_hex_char() {
        let mut s = String::from("blake3:");
        s.push_str(&"0".repeat(63));
        s.push('g');
        assert!(matches!(
            s.parse::<Digest>(),
            Err(DigestParseError::InvalidHex('g', 63))
        ));
    }

    #[test]
    fn rejects_wrong_len() {
        assert!(matches!(
            "blake3:abcd".parse::<Digest>(),
            Err(DigestParseError::WrongLen(4))
        ));
        let short = format!("blake3:{}", "a".repeat(63));
        assert!(matches!(
            short.parse::<Digest>(),
            Err(DigestParseError::WrongLen(63))
        ));
    }

    #[test]
    fn serde_roundtrip() {
        let d = Digest::new(b"x");
        let s = serde_json::to_string(&d).unwrap();
        assert_eq!(s, format!("\"{d}\""));
        let back: Digest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
    }
}
