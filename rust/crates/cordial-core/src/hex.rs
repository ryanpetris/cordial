//! Hex strings for fixed-size Bluetooth keys in persisted JSON.
use serde::{Deserialize, Deserializer, Serializer, de::Error};
pub fn serialize<S: Serializer, const N: usize>(
    bytes: &[u8; N],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    struct Hex<'a>(&'a [u8]);
    impl core::fmt::Display for Hex<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for byte in self.0 {
                write!(f, "{byte:02x}")?;
            }
            Ok(())
        }
    }
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
