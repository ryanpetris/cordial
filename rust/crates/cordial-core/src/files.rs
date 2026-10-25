//! Shared debug and assisted file commands. All argument checks precede mutations.
use crate::{
    model::storage_path,
    storage::{Error, RecordStore},
};
use alloc::vec::Vec;
use cordial_protocol as p;

pub fn failure(error: Error, mutation: bool) -> p::Error {
    p::Error {
        code: match error {
            Error::Bounds | Error::TooLarge => p::ErrorCode::BadArgs,
            Error::Missing => p::ErrorCode::NotFound,
            Error::Full => p::ErrorCode::NoCapacity,
            _ => p::ErrorCode::StorageFailed,
        } as i32,
        reason: p::ErrorReason::Storage as i32,
        outcome: if mutation && error == Error::Unknown {
            p::ErrorOutcome::Unknown
        } else {
            p::ErrorOutcome::NoEffect
        } as i32,
        ..Default::default()
    }
}
fn path(value: &str) -> Result<(), Error> {
    if value == "/" || crate::storage::is_write_temp(value) || !storage_path(value) {
        Err(Error::Bounds)
    } else {
        Ok(())
    }
}
pub async fn read<S: RecordStore>(store: &mut S, args: p::ReadFile) -> Result<p::FileData, Error> {
    path(&args.path)?;
    if args.length.is_some_and(|n| n == 0 || n > 512)
        || args.length.is_none()
        || args.offset > u64::from(u32::MAX)
    {
        return Err(Error::Bounds);
    }
    let size = store.file_size(&args.path).await?.ok_or(Error::Missing)?;
    let offset = usize::try_from(args.offset).map_err(|_| Error::Bounds)?;
    if offset > size {
        return Err(Error::Bounds);
    }
    let length = args
        .length
        .map_or(size, |n| (n as usize).min(size - offset));
    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .map_err(|_| Error::Unavailable)?;
    data.resize(length, 0);
    let mut at = 0;
    while at < length {
        let end = length.min(at + 512);
        let pos = u32::try_from(offset + at).map_err(|_| Error::Bounds)?;
        let count = store.file_read(&args.path, pos, &mut data[at..end]).await?;
        if count == 0 || count > end - at {
            return Err(Error::Io);
        }
        at += count;
    }
    Ok(p::FileData {
        path: args.path,
        offset: args.offset,
        size: size as u64,
        data,
    })
}
pub async fn write<S: RecordStore>(store: &mut S, args: p::WriteFile) -> Result<(), Error> {
    path(&args.path)?;
    if args.data.len() > 512
        || args
            .offset
            .checked_add(args.data.len() as u64)
            .is_none_or(|n| n > u64::from(u32::MAX))
    {
        return Err(Error::Bounds);
    }
    store
        .file_write(&args.path, args.offset as u32, &args.data)
        .await
}
pub async fn move_file<S: RecordStore>(store: &mut S, args: p::MoveFile) -> Result<(), Error> {
    path(&args.source)?;
    path(&args.destination)?;
    store.file_move(&args.source, &args.destination).await
}
pub async fn delete<S: RecordStore>(store: &mut S, args: p::DeleteFile) -> Result<(), Error> {
    path(&args.path)?;
    if args.path == "/format.pb" {
        return Err(Error::Bounds);
    }
    store.file_delete(&args.path).await
}

/// Whole-file debug response with a 512-byte read buffer.
pub struct Reader {
    path: alloc::string::String,
    size: u32,
    offset: u32,
    started: bool,
    error: Option<Error>,
}
impl Reader {
    pub async fn new<S: RecordStore>(store: &mut S, args: p::ReadFile) -> Result<Self, Error> {
        path(&args.path)?;
        if args.offset != 0 || args.length.is_some() {
            return Err(Error::Bounds);
        }
        let size = store.file_size(&args.path).await?.ok_or(Error::Missing)?;
        Ok(Self {
            path: args.path,
            size: u32::try_from(size).map_err(|_| Error::TooLarge)?,
            offset: 0,
            started: false,
            error: None,
        })
    }
    fn header(&self) -> Result<Vec<u8>, Error> {
        use prost::encoding::{WireType, encode_key, encode_varint, encoded_len_varint};
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(self.path.len() + 32)
            .map_err(|_| Error::Unavailable)?;
        prost::encoding::string::encode(1, &self.path, &mut fields);
        prost::encoding::uint64::encode(3, &(self.size as u64), &mut fields);
        encode_key(4, WireType::LengthDelimited, &mut fields);
        encode_varint(self.size.into(), &mut fields);
        let file = fields.len() as u64 + u64::from(self.size);
        let response = 1 + encoded_len_varint(file) as u64 + file;
        let mut header = Vec::new();
        header
            .try_reserve_exact(fields.len() + 22)
            .map_err(|_| Error::Unavailable)?;
        encode_key(1, WireType::LengthDelimited, &mut header);
        encode_varint(response, &mut header);
        encode_key(11, WireType::LengthDelimited, &mut header);
        encode_varint(file, &mut header);
        header.extend_from_slice(&fields);
        Ok(header)
    }
    /// Returns true once the complete response has been queued.
    pub async fn step<S: RecordStore>(
        &mut self,
        serial: &mut crate::control::Session,
        store: &mut S,
    ) -> bool {
        if !self.started {
            match self.header() {
                Ok(header) => serial.stream_bytes(&header, self.size == 0),
                Err(error) => {
                    serial.fail(failure(error, false));
                    return true;
                }
            }
            self.started = true;
            return self.size == 0;
        }
        if self.offset == self.size {
            serial.fail(failure(self.error.unwrap(), false));
            return true;
        }
        let mut bytes = [0; 512];
        let length = (self.size - self.offset).min(512) as usize;
        if self.error.is_none() {
            let mut read = 0;
            while read < length {
                match store
                    .file_read(
                        &self.path,
                        self.offset + read as u32,
                        &mut bytes[read..length],
                    )
                    .await
                {
                    Ok(n) if n != 0 && n <= length - read => read += n,
                    Ok(_) => {
                        self.error = Some(Error::Io);
                        break;
                    }
                    Err(error) => {
                        self.error = Some(error);
                        break;
                    }
                }
            }
        }
        self.offset += length as u32;
        if self.offset == self.size && self.error.is_none() {
            match store.file_size(&self.path).await {
                Ok(Some(size)) if size == self.size as usize => (),
                Ok(_) => self.error = Some(Error::Io),
                Err(error) => self.error = Some(error),
            }
        }
        // After a read failure, padding closes the declared bytes field and enclosing messages.
        // The final error envelope then replaces the file under protobuf's oneof rules.
        let finished = self.offset == self.size && self.error.is_none();
        serial.stream_bytes(&bytes[..length], finished);
        finished
    }
}
