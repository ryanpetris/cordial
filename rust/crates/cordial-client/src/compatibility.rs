//! Host compatibility with the adapter's native serial protocol.
use cordial_protocol::{AdapterHello, CURRENT_PROTOCOL_VERSION, handshake::decode_adapter_hello};

/// A validated adapter hello and its original decoded bytes, including unknown fields.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeHello {
    pub hello: AdapterHello,
    pub bytes: Vec<u8>,
}

impl NativeHello {
    /// Validates the permanent handshake without requiring a supported native revision.
    pub fn decode(bytes: &[u8]) -> Result<Self, cordial_protocol::handshake::HandshakeError> {
        Ok(Self {
            hello: decode_adapter_hello(bytes)?,
            bytes: bytes.to_vec(),
        })
    }

    pub fn compatibility(&self) -> Compatibility {
        compatibility(self.hello.protocol_version.unwrap_or(0))
    }
}

/// Recognizes the stable ordinary message envelope without decoding its native payload.
/// Discovery discards these messages until a valid adapter identity selects the codec.
pub fn is_ordinary_message(mut bytes: &[u8]) -> bool {
    use prost::encoding::{WireType, decode_key, decode_varint};
    let mut seen = false;
    while !bytes.is_empty() {
        let Ok((tag, WireType::LengthDelimited)) = decode_key(&mut bytes) else {
            return false;
        };
        if !matches!(tag, 1 | 2) {
            return false;
        }
        let Ok(length) = decode_varint(&mut bytes) else {
            return false;
        };
        let Ok(length) = usize::try_from(length) else {
            return false;
        };
        let Some(rest) = bytes.get(length..) else {
            return false;
        };
        bytes = rest;
        seen = true;
    }
    seen
}

/// Whether this host implements the adapter's native revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Compatibility {
    Current,
    Newer,
    Unsupported,
}

/// Only the current revision has an implemented path. Zero is never a valid hello version.
pub fn compatibility(native: u32) -> Compatibility {
    match native.cmp(&CURRENT_PROTOCOL_VERSION) {
        std::cmp::Ordering::Equal => Compatibility::Current,
        std::cmp::Ordering::Greater => Compatibility::Newer,
        std::cmp::Ordering::Less => Compatibility::Unsupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_compiled_revision_is_supported() {
        assert_eq!(
            compatibility(CURRENT_PROTOCOL_VERSION),
            Compatibility::Current
        );
        assert_eq!(
            compatibility(CURRENT_PROTOCOL_VERSION + 1),
            Compatibility::Newer
        );
        assert_eq!(compatibility(0), Compatibility::Unsupported);
        assert_eq!(CURRENT_PROTOCOL_VERSION, 1);
    }
}
