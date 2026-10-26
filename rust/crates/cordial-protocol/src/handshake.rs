//! Permanent handshake messages and strict validation of decoded protobuf bytes.
use alloc::vec::Vec;
use prost::{
    Message,
    encoding::{DecodeContext, WireType, decode_key, decode_varint, skip_field},
};

#[allow(clippy::all, clippy::pedantic, missing_docs)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/cordial.handshake.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.handshake.serde.rs"));
}
pub use crate::CURRENT_PROTOCOL_VERSION;
pub use generated::*;

/// Maximum decoded protobuf size of either hello, before COBS encoding.
pub const MAX_HANDSHAKE_BYTES: usize = crate::MAX_REQUEST_BYTES;

/// Why a hello cannot be used. A valid version may differ from this build's revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    /// The decoded message exceeds the handshake limit.
    TooLong,
    /// No top-level protocol version is present.
    MissingVersion,
    /// Zero is not a protocol revision.
    ZeroVersion,
    /// More than one protocol version is present, even if their values agree.
    DuplicateVersion,
    /// The version has a wrong wire type, overflows uint32, or is truncated.
    InvalidVersion,
    /// The protobuf structure or a known metadata field is invalid.
    Malformed,
}

// Match uint32 key encodings in the TypeScript protobuf decoder, including inside groups.
fn key(bytes: &mut &[u8]) -> Result<(u32, WireType), HandshakeError> {
    let length = bytes.len();
    let key = decode_key(bytes).map_err(|_| HandshakeError::Malformed)?;
    if length - bytes.len() > 5 {
        return Err(HandshakeError::Malformed);
    }
    Ok(key)
}

fn skip(wire: WireType, tag: u32, bytes: &mut &[u8], depth: usize) -> Result<(), HandshakeError> {
    if depth == 100 {
        return Err(HandshakeError::Malformed);
    }
    if wire != WireType::StartGroup {
        return skip_field(wire, tag, bytes, DecodeContext::default())
            .map_err(|_| HandshakeError::Malformed);
    }
    loop {
        let (inner_tag, inner_wire) = key(bytes)?;
        if inner_wire == WireType::EndGroup {
            return if inner_tag == tag {
                Ok(())
            } else {
                Err(HandshakeError::Malformed)
            };
        }
        skip(inner_wire, inner_tag, bytes, depth + 1)?;
    }
}

fn validate(mut bytes: &[u8]) -> Result<(), HandshakeError> {
    use HandshakeError::*;
    if bytes.len() > MAX_HANDSHAKE_BYTES {
        return Err(TooLong);
    }
    let mut version = None;
    while !bytes.is_empty() {
        let (tag, wire) = key(&mut bytes)?;
        if tag == 1 {
            if version.is_some() {
                return Err(DuplicateVersion);
            }
            if wire != WireType::Varint {
                return Err(InvalidVersion);
            }
            let value = decode_varint(&mut bytes).map_err(|_| InvalidVersion)?;
            let value = u32::try_from(value).map_err(|_| InvalidVersion)?;
            if value == 0 {
                return Err(ZeroVersion);
            }
            version = Some(value);
        } else {
            skip(wire, tag, &mut bytes, 0)?;
        }
    }
    version.ok_or(MissingVersion).map(|_| ())
}

/// Decodes the fixed empty identification query. Nonempty bytes are not a host hello.
pub fn decode_client_hello(bytes: &[u8]) -> Result<ClientHello, HandshakeError> {
    if bytes.len() > MAX_HANDSHAKE_BYTES {
        return Err(HandshakeError::TooLong);
    }
    if !bytes.is_empty() {
        return Err(HandshakeError::Malformed);
    }
    Ok(ClientHello {})
}

/// Validates and decodes an adapter hello. All metadata is optional.
pub fn decode_adapter_hello(bytes: &[u8]) -> Result<AdapterHello, HandshakeError> {
    validate(bytes)?;
    AdapterHello::decode(bytes).map_err(|_| HandshakeError::Malformed)
}

/// Encodes the fixed empty identification query. Pass it to [`crate::frame::encode_bytes`].
pub fn encode_client_hello(hello: &ClientHello) -> Result<Vec<u8>, HandshakeError> {
    Ok(hello.encode_to_vec())
}

/// Encodes and validates an adapter hello, including the metadata's total size.
pub fn encode_adapter_hello(hello: &AdapterHello) -> Result<Vec<u8>, HandshakeError> {
    if hello.encoded_len() > MAX_HANDSHAKE_BYTES {
        return Err(HandshakeError::TooLong);
    }
    let bytes = hello.encode_to_vec();
    validate(&bytes)?;
    Ok(bytes)
}
