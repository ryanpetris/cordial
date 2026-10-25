//! Frames on the serial port: each message is protobuf-encoded, then COBS-encoded, then followed
//! by one zero byte. COBS output contains no zero byte, so a zero always ends a frame and a
//! receiver that starts mid-stream resynchronizes at the next one.
use alloc::vec::Vec;

use prost::Message;

/// The frame delimiter. A client sends one before its first request so that bytes left by an
/// earlier client cannot merge with it.
pub const DELIMITER: u8 = 0;

/// Appends the frame for `message` to `out`.
pub fn encode<M: Message>(message: &M, out: &mut Vec<u8>) {
    let bytes = message.encode_to_vec();
    encode_bytes(&bytes, out);
}

/// Appends the COBS encoding of `bytes` and the delimiter to `out`.
pub fn encode_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    out.reserve(bytes.len() + bytes.len() / 254 + 2);
    let mut code_at = out.len();
    out.push(0);
    let mut code = 1u8;
    for &byte in bytes {
        if byte == 0 {
            out[code_at] = code;
            code_at = out.len();
            out.push(0);
            code = 1;
            continue;
        }
        out.push(byte);
        code += 1;
        if code == 0xff {
            out[code_at] = code;
            code_at = out.len();
            out.push(0);
            code = 1;
        }
    }
    out[code_at] = code;
    out.push(DELIMITER);
}

/// Incremental COBS encoding of one frame, retaining at most one 254-byte block.
pub struct Encoder {
    block: [u8; 254],
    length: usize,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    /// Starts an empty frame.
    pub fn new() -> Self {
        Self {
            block: [0; 254],
            length: 0,
        }
    }

    /// Appends complete COBS blocks. Calls may split the input anywhere.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &byte in bytes {
            if byte == 0 {
                self.flush(out);
            } else {
                self.block[self.length] = byte;
                self.length += 1;
                if self.length == self.block.len() {
                    self.flush(out);
                }
            }
        }
    }

    fn flush(&mut self, out: &mut Vec<u8>) {
        out.push((self.length + 1) as u8);
        out.extend_from_slice(&self.block[..self.length]);
        self.length = 0;
    }

    /// Ends the frame with the remaining block and its delimiter.
    pub fn finish(mut self, out: &mut Vec<u8>) {
        self.flush(out);
        out.push(DELIMITER);
    }
}

/// Why a received frame could not be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The decoded frame is longer than the decoder's limit.
    TooLong,
    /// The bytes are not valid COBS.
    Malformed,
}

/// Collects received bytes into frames. Empty frames are skipped.
pub struct Decoder {
    buffer: Vec<u8>,
    limit: Option<usize>,
    discarding: bool,
    /// The buffer holds the frame returned by the last push.
    returned: bool,
}

impl Decoder {
    /// A decoder that rejects frames longer than `limit` bytes once decoded, or accepts any length
    /// when `limit` is `None`.
    pub fn new(limit: Option<usize>) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            discarding: false,
            returned: false,
        }
    }

    /// The most encoded bytes a frame within the limit can take.
    fn encoded_limit(&self) -> Option<usize> {
        self.limit.map(|limit| limit + limit / 254 + 1)
    }

    /// Drops any partial frame, as at the start of a new session.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.discarding = false;
        self.returned = false;
    }

    /// Adds one received byte. Returns a decoded frame, or the reason it was dropped, when `byte`
    /// ends one.
    pub fn push(&mut self, byte: u8) -> Option<Result<&[u8], FrameError>> {
        if core::mem::take(&mut self.returned) {
            self.buffer.clear();
        }
        if byte != DELIMITER {
            if self.discarding {
                return None;
            }
            if self
                .encoded_limit()
                .is_some_and(|limit| self.buffer.len() >= limit)
            {
                self.discarding = true;
                self.buffer.clear();
                return None;
            }
            self.buffer.push(byte);
            return None;
        }
        if core::mem::take(&mut self.discarding) {
            return Some(Err(FrameError::TooLong));
        }
        if self.buffer.is_empty() {
            return None;
        }
        let result = decode_in_place(&mut self.buffer);
        let length = match result {
            Ok(length) => length,
            Err(error) => {
                self.buffer.clear();
                return Some(Err(error));
            }
        };
        if self.limit.is_some_and(|limit| length > limit) {
            self.buffer.clear();
            return Some(Err(FrameError::TooLong));
        }
        self.buffer.truncate(length);
        self.returned = true;
        Some(Ok(&self.buffer))
    }
}

/// Decodes COBS bytes without a delimiter in place, returning the decoded length.
fn decode_in_place(bytes: &mut [u8]) -> Result<usize, FrameError> {
    let mut read = 0;
    let mut write = 0;
    while read < bytes.len() {
        let code = bytes[read] as usize;
        if code == 0 || read + code > bytes.len() {
            return Err(FrameError::Malformed);
        }
        read += 1;
        for _ in 1..code {
            bytes[write] = bytes[read];
            write += 1;
            read += 1;
        }
        if code < 0xff && read < bytes.len() {
            bytes[write] = 0;
            write += 1;
        }
    }
    Ok(write)
}

#[cfg(test)]
mod tests {
    use super::{DELIMITER, Decoder, FrameError};

    #[test]
    fn an_unterminated_overlong_frame_stops_growing_the_buffer() {
        let mut decoder = Decoder::new(Some(16));
        for _ in 0..18 {
            assert!(decoder.push(1).is_none());
        }
        assert!(decoder.discarding);
        assert!(decoder.buffer.is_empty());
        let capacity = decoder.buffer.capacity();

        for _ in 0..65_536 {
            assert!(decoder.push(1).is_none());
        }
        assert!(decoder.buffer.is_empty());
        assert_eq!(decoder.buffer.capacity(), capacity);
        assert_eq!(decoder.push(DELIMITER), Some(Err(FrameError::TooLong)));
    }
}
