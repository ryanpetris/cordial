//! File-backed storage messages and format validation independent of their payload schema.

use alloc::vec::Vec;
use prost::encoding::{WireType, decode_key, decode_varint};

/// Operating format 2 records. The same types are re-exported directly from this module.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod v2 {
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.v2.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.v2.serde.rs"));
}
pub use v2::*;

/// Historical format 1 records for host-side conversion. Firmware does not enable this feature.
#[cfg(feature = "historical-storage")]
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.v1.rs"));
    #[cfg(feature = "json")]
    include!(concat!(env!("OUT_DIR"), "/cordial.storage.v1.serde.rs"));
}

/// The format written by every storage file writer.
pub const CURRENT_FORMAT: u32 = 2;

/// A file's format cannot be read or is not supported by this build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatError {
    /// The protobuf framing or its single nonzero uint32 format field is invalid.
    Malformed,
    /// The file declares another format.
    Unsupported(u32),
}

#[derive(Default)]
enum Field {
    #[default]
    Key,
    Varint(bool),
    Length,
}

/// Reads a file's format while skipping payloads without buffering them.
#[derive(Default)]
pub struct FormatReader {
    field: Field,
    pending: [u8; 10],
    used: usize,
    skip: u64,
    groups: Vec<u32>,
    format: Option<u32>,
}

impl FormatReader {
    /// Scans the next bytes of a file. Parts may end anywhere within a field.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<(), FormatError> {
        use FormatError::Malformed;
        while !bytes.is_empty() {
            if self.skip != 0 {
                let n = self.skip.min(bytes.len() as u64) as usize;
                bytes = &bytes[n..];
                self.skip -= n as u64;
                continue;
            }
            let byte = bytes[0];
            bytes = &bytes[1..];
            self.pending[self.used] = byte;
            self.used += 1;
            if byte & 0x80 != 0 {
                if self.used == self.pending.len() {
                    return Err(Malformed);
                }
                continue;
            }
            let mut value = &self.pending[..self.used];
            self.used = 0;
            match self.field {
                Field::Key => {
                    let (tag, wire) = decode_key(&mut value).map_err(|_| Malformed)?;
                    let header = tag == 1 && self.groups.is_empty();
                    if header && wire != WireType::Varint {
                        return Err(Malformed);
                    }
                    match wire {
                        WireType::Varint => self.field = Field::Varint(header),
                        WireType::LengthDelimited => self.field = Field::Length,
                        WireType::SixtyFourBit => self.skip = 8,
                        WireType::ThirtyTwoBit => self.skip = 4,
                        WireType::StartGroup => {
                            if self.groups.len() == 100 {
                                return Err(Malformed);
                            }
                            self.groups.push(tag);
                        }
                        WireType::EndGroup => {
                            if self.groups.pop() != Some(tag) {
                                return Err(Malformed);
                            }
                        }
                    }
                }
                Field::Varint(header) => {
                    let value = decode_varint(&mut value).map_err(|_| Malformed)?;
                    if header {
                        let value = u32::try_from(value).map_err(|_| Malformed)?;
                        if value == 0 || self.format.replace(value).is_some() {
                            return Err(Malformed);
                        }
                    }
                    self.field = Field::Key;
                }
                Field::Length => {
                    self.skip = decode_varint(&mut value).map_err(|_| Malformed)?;
                    self.field = Field::Key;
                }
            }
        }
        Ok(())
    }

    /// Returns the declared version after the entire file has been scanned.
    pub fn finish(self) -> Result<u32, FormatError> {
        if self.used != 0
            || self.skip != 0
            || !self.groups.is_empty()
            || !matches!(self.field, Field::Key)
        {
            return Err(FormatError::Malformed);
        }
        self.format.ok_or(FormatError::Malformed)
    }
}

/// Extracts exactly one nonzero uint32 format field without interpreting the payload schema.
///
/// Only `/format.pb` reliably identifies a dataset's source schema. Format 1 payloads do not
/// carry a format header: `Sequence.next_device = 2` also encodes as tag 1 with value 2.
/// Apply this reader to other files only after the root selects a schema with per-file headers.
pub fn extract_format(bytes: &[u8]) -> Result<u32, FormatError> {
    let mut reader = FormatReader::default();
    reader.feed(bytes)?;
    reader.finish()
}

/// Checks whether the declared format is supported.
pub fn check_format(format: u32) -> Result<(), FormatError> {
    if format == CURRENT_FORMAT {
        Ok(())
    } else {
        Err(FormatError::Unsupported(format))
    }
}

/// Validates the file's format before its payload is decoded.
pub fn validate_format(bytes: &[u8]) -> Result<(), FormatError> {
    check_format(extract_format(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::{Message, encoding};

    #[test]
    fn format_is_independent_of_payload_tags_and_order() {
        let mut payload = Vec::new();
        encoding::bytes::encode(41, &alloc::vec![8, 99, 0xff], &mut payload);
        encoding::uint32::encode(400, &123, &mut payload);
        encoding::fixed32::encode(90, &42, &mut payload);
        encoding::fixed64::encode(91, &42, &mut payload);
        // A group can contain its own field 1 without declaring the file's format.
        encoding::encode_key(92, WireType::StartGroup, &mut payload);
        encoding::uint32::encode(1, &99, &mut payload);
        encoding::encode_key(92, WireType::EndGroup, &mut payload);
        let header = Format {
            format: CURRENT_FORMAT,
            initialized: false,
        }
        .encode_to_vec();
        for bytes in [
            [header.clone(), payload.clone()].concat(),
            [payload.clone(), header.clone()].concat(),
            [payload.clone(), header, payload].concat(),
        ] {
            assert_eq!(extract_format(&bytes), Ok(CURRENT_FORMAT));
            assert_eq!(validate_format(&bytes), Ok(()));
            for split in 0..=bytes.len() {
                let mut reader = FormatReader::default();
                reader.feed(&bytes[..split]).unwrap();
                reader.feed(&bytes[split..]).unwrap();
                assert_eq!(reader.finish(), Ok(CURRENT_FORMAT));
            }
            let mut reader = FormatReader::default();
            for byte in &bytes {
                reader.feed(core::slice::from_ref(byte)).unwrap();
            }
            assert_eq!(reader.finish(), Ok(CURRENT_FORMAT));
        }
    }

    #[test]
    fn invalid_headers_and_framing_are_rejected_in_whole_files_and_parts() {
        let malformed: &[&[u8]] = &[
            &[],                                                           // Missing header.
            &[8, 0],                                                       // Zero.
            &[8, 2, 8, 2],                                                 // Duplicate.
            &[8, 2, 8, 3],                                                 // Conflicting duplicate.
            &[10, 1, 2],                                                   // Wrong wire type.
            &[8],                                                          // Truncated header.
            &[8, 0x80],                                                    // Truncated varint.
            &[8, 0x80, 0x80, 0x80, 0x80, 0x10],                            // uint32 overflow.
            &[8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2], // uint64 overflow.
            &[8, 2, 18, 2, 0],                                             // Truncated payload.
            &[8, 2, 0],                                                    // Invalid tag.
            &[8, 2, 22],                                                   // Invalid wire type.
            &[8, 2, 21, 0],                                                // Truncated fixed32.
            &[8, 2, 17, 0],                                                // Truncated fixed64.
            &[8, 2, 19],                                                   // Unclosed group.
            &[8, 2, 19, 28],                                               // Mismatched end group.
            &[8, 2, 20],                                                   // Unexpected end group.
        ];
        for bytes in malformed {
            assert_eq!(
                extract_format(bytes),
                Err(FormatError::Malformed),
                "{bytes:?}"
            );
            let mut reader = FormatReader::default();
            let result = bytes
                .iter()
                .try_for_each(|byte| reader.feed(core::slice::from_ref(byte)))
                .and_then(|()| reader.finish());
            assert_eq!(result, Err(FormatError::Malformed), "{bytes:?}");
        }
        for format in [1, 3, u32::MAX] {
            let mut bytes = Vec::new();
            encoding::uint32::encode(1, &format, &mut bytes);
            assert_eq!(extract_format(&bytes), Ok(format));
            assert_eq!(
                validate_format(&bytes),
                Err(FormatError::Unsupported(format))
            );
        }
    }
}
