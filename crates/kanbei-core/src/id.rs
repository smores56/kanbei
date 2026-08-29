//! Branded 128-bit ids: a bare base58 [`Id128`] (16-byte UUIDv7, Bitcoin
//! alphabet, stable 21-char width) plus a brand prefix that names the object
//! class (`ses_`, `br_`, `ev_`). Parity source: maki-storage/src/id.rs;
//! width ratified in docs/spikes/ratification-packet.md §5. Kanbei does not
//! accept legacy hex UUIDs on parse — the text form is frozen at M1.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

pub const UUID_BYTES: usize = 16;

/// The canonical id for anything in kanbei (sessions, branches, events):
/// time-ordered, base58-encoded, backed by a UUIDv7.
///
/// Base58 is variable-length (21-22 chars for 16 bytes); v7 ids always carry
/// a nonzero high byte, so they encode to a stable 21 chars and lexical sort
/// orders them chronologically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id128([u8; UUID_BYTES]);

impl Id128 {
    pub fn generate() -> Self {
        Self(Uuid::now_v7().into_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; UUID_BYTES] {
        &self.0
    }
}

impl fmt::Display for Id128 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&bs58::encode(&self.0).into_string())
    }
}

impl FromStr for Id128 {
    type Err = Id128ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(Id128ParseError::Empty);
        }
        decode_base58(s)
    }
}

fn decode_base58(s: &str) -> Result<Id128, Id128ParseError> {
    let bytes = bs58::decode(s).into_vec().map_err(|e| match e {
        bs58::decode::Error::InvalidCharacter { character, index } => {
            Id128ParseError::InvalidBase58(character, index)
        }
        bs58::decode::Error::NonAsciiCharacter { index } => {
            Id128ParseError::InvalidBase58('\u{FFFD}', index)
        }
        _ => Id128ParseError::InvalidBase58Length,
    })?;
    if bytes.len() != UUID_BYTES {
        return Err(Id128ParseError::InvalidByteLen(bytes.len()));
    }
    let mut arr = [0u8; UUID_BYTES];
    arr.copy_from_slice(&bytes);
    Ok(Id128(arr))
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Id128ParseError {
    #[error("empty id")]
    Empty,
    #[error("invalid base58 character {0:?} at {1}")]
    InvalidBase58(char, usize),
    #[error("base58 string has a length that cannot decode to whole bytes")]
    InvalidBase58Length,
    #[error("id decoded to {0} bytes, expected {UUID_BYTES}")]
    InvalidByteLen(usize),
}

impl Serialize for Id128 {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Id128 {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Known brand prefixes, longest-first so a prefix match is unambiguous.
/// (All current brands are the same length; order is still significant if a
/// brand ever becomes a prefix of another.)
pub const BRANDS: &[&str] = &["ses_", "br_", "ev_", "mod_", "gen_"];

/// An [`Id128`] with its object class named by a [`BRANDS`] prefix.
/// Text form is `{brand}{id}`, e.g. `ses_` + 21 base58 chars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrandedId {
    brand: &'static str,
    id: Id128,
}

impl BrandedId {
    pub fn new(brand: &'static str, id: Id128) -> Self {
        Self { brand, id }
    }

    pub fn id(&self) -> Id128 {
        self.id
    }

    pub fn brand(&self) -> &'static str {
        self.brand
    }
}

impl fmt::Display for BrandedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.brand, self.id)
    }
}

/// Shared parse core. `expected = None` accepts any known brand prefix and the
/// matched prefix becomes the id's brand; `expected = Some` requires that
/// exact brand (yielding [`BrandedParseError::WrongBrand`] otherwise).
fn parse_with(s: &str, expected: Option<&str>) -> Result<BrandedId, BrandedParseError> {
    let matched = BRANDS
        .iter()
        .find(|b| s.starts_with(**b))
        .copied()
        .ok_or_else(|| BrandedParseError::MissingBrand(s.to_string()))?;
    if let Some(expected) = expected
        && matched != expected
    {
        return Err(BrandedParseError::WrongBrand(
            matched.to_string(),
            expected.to_string(),
        ));
    }
    let id = s[matched.len()..].parse().map_err(BrandedParseError::Id)?;
    Ok(BrandedId { brand: matched, id })
}

/// Typed parse: the string must carry exactly the brand `brand`.
#[cfg(test)]
fn parse_brand(s: &str, brand: &str) -> Result<BrandedId, BrandedParseError> {
    parse_with(s, Some(brand))
}

impl FromStr for BrandedId {
    type Err = BrandedParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_with(s, None)
    }
}

/// Generic parse: accepts any [`BRANDS`] prefix and returns the id with that
/// brand.
pub fn parse_branded_any(s: &str) -> Result<BrandedId, BrandedParseError> {
    s.parse()
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrandedParseError {
    #[error("no known brand prefix in {0:?}")]
    MissingBrand(String),
    #[error("expected brand {1:?}, found {0:?}")]
    WrongBrand(String, String),
    #[error(transparent)]
    Id(#[from] Id128ParseError),
}

impl Serialize for BrandedId {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for BrandedId {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn generate_is_v7() {
        let id = Id128::generate();
        let uuid = Uuid::from_bytes(id.0);
        assert_eq!(uuid.get_version(), Some(uuid::Version::SortRand));
    }

    #[test]
    fn roundtrip_base58() {
        let id = Id128::generate();
        let s = id.to_string();
        // v7 ids carry a nonzero high byte: stable 21 chars
        assert_eq!(s.len(), 21);
        assert_eq!(s.parse::<Id128>().unwrap(), id);
    }

    #[test]
    fn roundtrips_leading_zero_bytes() {
        let id = Id128([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let s = id.to_string();
        assert!((21..=22).contains(&s.len())); // leading zero byte -> leading '1'
        assert_eq!(s.parse::<Id128>().unwrap(), id);
        let zero = Id128([0u8; 16]);
        assert_eq!(zero.to_string().parse::<Id128>().unwrap(), zero);
    }

    #[test]
    fn rejects_bad_strings() {
        assert!(matches!("".parse::<Id128>(), Err(Id128ParseError::Empty)));
        assert!(matches!(
            "O".parse::<Id128>(),
            Err(Id128ParseError::InvalidBase58('O', 0))
        ));
        assert!(matches!(
            "2j87v4grC".parse::<Id128>(),
            Err(Id128ParseError::InvalidByteLen(_))
        ));
    }

    #[test]
    fn serde_roundtrip_base58() {
        let id = Id128::generate();
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, format!("\"{id}\""));
        let back: Id128 = serde_json::from_str(&s).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn branded_roundtrip_each_brand() {
        for brand in BRANDS {
            let id = Id128::generate();
            let b = BrandedId::new(brand, id);
            assert_eq!(b.brand(), *brand);
            assert_eq!(b.id(), id);
            let s = b.to_string();
            assert!(s.starts_with(brand));
            assert_eq!(s.parse::<BrandedId>().unwrap(), b);
        }
    }

    #[test]
    fn wrong_brand_error() {
        let id = Id128::generate();
        let err = parse_brand(&format!("ses_{id}"), "br_").unwrap_err();
        assert!(matches!(
            err,
            BrandedParseError::WrongBrand(found, expected)
                if found == "ses_" && expected == "br_"
        ));
    }

    #[test]
    fn missing_brand_error() {
        let err = "noprefix".parse::<BrandedId>().unwrap_err();
        assert!(matches!(err, BrandedParseError::MissingBrand(_)));
    }

    #[test]
    fn parse_branded_any_accepts_all_brands() {
        for brand in BRANDS {
            let b = BrandedId::new(brand, Id128::generate());
            let parsed = parse_branded_any(&b.to_string()).unwrap();
            assert_eq!(parsed, b);
        }
    }

    #[test]
    fn branded_serde_roundtrip() {
        let b = BrandedId::new("ev_", Id128::generate());
        let s = serde_json::to_string(&b).unwrap();
        let back: BrandedId = serde_json::from_str(&s).unwrap();
        assert_eq!(back, b);
    }
}
