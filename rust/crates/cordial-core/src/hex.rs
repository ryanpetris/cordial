//! Hex strings for Bluetooth keys and byte strings in persisted JSON.
use serde::{Deserialize, Deserializer, Serializer, de::Error};
struct Hex<'a>(&'a [u8]);
impl core::fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
pub fn serialize<S: Serializer, const N: usize>(
    bytes: &[u8; N],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&Hex(bytes))
}
pub fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
    deserializer: D,
) -> Result<[u8; N], D::Error> {
    let value = <&str>::deserialize(deserializer)?;
    if value.len() != 2 * N || !value.is_ascii() {
        return Err(D::Error::custom("invalid hex length"));
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte =
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(D::Error::custom)?;
    }
    Ok(bytes)
}

/// Variable-length byte strings, such as saved report maps.
pub mod bytes {
    use super::Hex;
    use alloc::vec::Vec;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Hex(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let value = <&str>::deserialize(deserializer)?;
        if value.len() % 2 != 0 || !value.is_ascii() {
            return Err(D::Error::custom("invalid hex length"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(value.len() / 2)
            .map_err(D::Error::custom)?;
        for index in (0..value.len()).step_by(2) {
            bytes.push(u8::from_str_radix(&value[index..index + 2], 16).map_err(D::Error::custom)?);
        }
        Ok(bytes)
    }
}
