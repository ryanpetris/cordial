use crate::{
    MAX_LINE_BYTES, PROTOCOL_VERSION,
    identifiers::RequestId,
    messages::{Command, Request},
};
use alloc::{borrow::Cow, vec::Vec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Debug)]
pub enum DecodeError {
    TooLong,
    Json(serde_json::Error),
    InvalidRequest { supplied: Option<RequestId> },
    Version { id: RequestId },
    UnknownCommand { id: RequestId },
    Arguments { id: RequestId },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<'a> {
    v: u64,
    id: RequestId,
    #[serde(borrow)]
    cmd: Cow<'a, str>,
    #[serde(borrow)]
    args: &'a serde_json::value::RawValue,
}

pub fn decode_request(bytes: &[u8]) -> Result<Request, DecodeError> {
    if bytes.len() >= MAX_LINE_BYTES {
        return Err(DecodeError::TooLong);
    }
    let object = bytes.trim_ascii_start().starts_with(b"{");
    let envelope: Envelope<'_> = serde_json::from_slice(bytes).map_err(|error| {
        if error.is_data() {
            if !object {
                return DecodeError::InvalidRequest { supplied: None };
            }
            #[derive(Deserialize)]
            struct Supplied {
                id: Option<RequestId>,
            }
            let supplied = serde_json::from_slice::<Supplied>(bytes)
                .ok()
                .and_then(|s| s.id);
            DecodeError::InvalidRequest { supplied }
        } else {
            DecodeError::Json(error)
        }
    })?;
    if !object {
        return Err(DecodeError::InvalidRequest { supplied: None });
    }
    let id = envelope.id;
    if !envelope.args.get().starts_with('{') {
        return Err(DecodeError::InvalidRequest { supplied: Some(id) });
    }
    if envelope.v != u64::from(PROTOCOL_VERSION) {
        return Err(DecodeError::Version { id });
    }
    macro_rules! command {
        ($($name:literal => $variant:ident),+ $(,)?) => {
            match envelope.cmd.as_ref() {
                $($name => Command::$variant(serde_json::from_str(envelope.args.get()).map_err(|_| DecodeError::Arguments { id })?),)+
                _ => return Err(DecodeError::UnknownCommand { id }),
            }
        };
    }
    let command = command! {
        "adapter.status" => Status, "adapter.capabilities" => Capabilities, "adapter.wait_ready" => Ready, "session.heartbeat" => Heartbeat, "adapter.bootloader.enter" => Bootloader,
        "storage.list" => StorageList, "storage.read" => StorageRead,
        "session.monitor.set" => Monitor, "device.list" => Devices, "device.get" => Info, "device.info" => DeviceInfo, "device.info.refresh" => DeviceInfoRefresh, "discovery.scan" => Scan,
        "pairing.start" => Pair, "pairing.reply" => PairReply, "device.connect" => Connect, "device.disconnect" => Disconnect,
        "device.unpair" => Unpair, "device.enabled.set" => DeviceEnabled, "device.trusted.set" => DeviceTrusted, "device.blocked.set" => DeviceBlocked,
        "device.hidpp.set" => Hidpp, "adapter.platform.set" => Platform, "adapter.name.set" => Name, "hidpp.feature.list" => Features, "hidpp.setting.list" => Settings,
        "hidpp.setting.get" => SettingsGet, "hidpp.setting.set" => SettingsSet, "hidpp.setting.forget" => SettingsForget,
        "hidpp.setting.refresh" => SettingsRefresh, "hidpp.setting.apply" => SettingsApply, "request.cancel" => Cancel,
    };
    if !command.valid_arguments() {
        return Err(DecodeError::Arguments { id });
    }
    Ok(Request {
        v: PROTOCOL_VERSION,
        id,
        command,
    })
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    if bytes.len() >= MAX_LINE_BYTES {
        return Err(DecodeError::TooLong);
    }
    serde_json::from_slice(bytes).map_err(DecodeError::Json)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncodeError {
    TooLong,
    Capacity,
    Serialization,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, EncodeError> {
    // Encode into bounded caller-stack storage, then allocate exactly once.
    // Heap pressure can delay a reply without an infallible Vec growth.
    let mut buffer = [0; MAX_LINE_BYTES];
    let length =
        serde_json_core::to_slice(value, &mut buffer[..MAX_LINE_BYTES - 1]).map_err(|error| {
            match error {
                serde_json_core::ser::Error::BufferFull => EncodeError::TooLong,
                _ => EncodeError::Serialization,
            }
        })?;
    buffer[length] = b'\n';
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length + 1)
        .map_err(|_| EncodeError::Capacity)?;
    bytes.extend_from_slice(&buffer[..length + 1]);
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    TooLong,
}

/// Caller-owned storage avoids a maximum-size allocation per pending request.
pub struct Framer<'a> {
    buffer: &'a mut [u8],
    length: usize,
    discarding: bool,
}
impl<'a> Framer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            length: 0,
            discarding: false,
        }
    }
    pub fn reset(&mut self) {
        self.length = 0;
        self.discarding = false;
    }
    /// Process each returned line before supplying more bytes. Oversized input
    /// is discarded through LF, then the following line can be accepted.
    pub fn push(&mut self, byte: u8) -> Option<Result<&[u8], FrameError>> {
        if byte == b'\n' {
            let mut n = self.length;
            let overflow = self.discarding;
            self.reset();
            if !overflow {
                if n != 0 && self.buffer[n - 1] == b'\r' {
                    n -= 1;
                }
                if n == 0 {
                    return None;
                }
            }
            return Some(if overflow {
                Err(FrameError::TooLong)
            } else {
                Ok(&self.buffer[..n])
            });
        }
        if !self.discarding {
            if self.length >= self.buffer.len().min(MAX_LINE_BYTES - 1) {
                self.discarding = true;
                self.length = 0;
            } else {
                self.buffer[self.length] = byte;
                self.length += 1;
            }
        }
        None
    }
}
