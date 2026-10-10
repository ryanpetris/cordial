//! Byte helpers of the binary bond form native adapters exchange.
use crate::model::identifiers::Transport;
use crate::{devices::Peer, storage::Error};
use alloc::vec::Vec;

pub struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let (head, tail) = self.0.split_at_checked(n).ok_or(Error::Corrupt)?;
        self.0 = tail;
        Ok(head)
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn finish(self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::Corrupt)
        }
    }
    pub fn peer(&mut self) -> Result<Peer, Error> {
        let transport = match self.u8()? {
            1 => Transport::Classic,
            2 => Transport::Ble,
            _ => return Err(Error::Corrupt),
        };
        let random = match self.u8()? {
            0 => false,
            1 if transport == Transport::Ble => true,
            _ => return Err(Error::Corrupt),
        };
        Ok(Peer {
            transport,
            random,
            address: self.take(6)?.try_into().unwrap(),
        })
    }
}
pub fn peer(bytes: &mut Vec<u8>, peer: Peer) {
    bytes.push(match peer.transport {
        Transport::Classic => 1,
        Transport::Ble => 2,
    });
    bytes.push(u8::from(peer.random));
    bytes.extend_from_slice(&peer.address);
}
