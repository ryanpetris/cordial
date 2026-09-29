//! Portable bond records. Native adapters translate their database callbacks to
//! these fields; committed records are selected only by a device's bond ID.
use crate::{
    codec::{self, Reader},
    devices::Peer,
    storage::{Error, RecordStore, record_key},
};
use alloc::vec::Vec;
use cordial_protocol::identifiers::Transport;
pub const MAINTENANCE_BYTES: usize = 32768;
pub const PAIR_BYTES: usize = 16384;
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Security {
    /// LTK, IRK, CSRK present; authenticated, authorized, secure connections.
    pub flags: u8,
    pub key_size: u8,
    pub ediv: u16,
    #[serde(with = "crate::hex")]
    pub rand: [u8; 8],
    #[serde(with = "crate::hex")]
    pub ltk: [u8; 16],
    #[serde(with = "crate::hex")]
    pub irk: [u8; 16],
    #[serde(with = "crate::hex")]
    pub csrk: [u8; 16],
    pub counter: u32,
}
impl Security {
    fn valid(&self) -> bool {
        self.flags & !63 == 0 && (self.flags & 1 == 0 || (7..=16).contains(&self.key_size))
    }
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&[self.flags, self.key_size]);
        out.extend_from_slice(&self.ediv.to_le_bytes());
        out.extend_from_slice(&self.rand);
        out.extend_from_slice(&self.ltk);
        out.extend_from_slice(&self.irk);
        out.extend_from_slice(&self.csrk);
        out.extend_from_slice(&self.counter.to_le_bytes());
    }
    fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let v = Self {
            flags: r.u8()?,
            key_size: r.u8()?,
            ediv: r.u16()?,
            rand: r.take(8)?.try_into().unwrap(),
            ltk: r.take(16)?.try_into().unwrap(),
            irk: r.take(16)?.try_into().unwrap(),
            csrk: r.take(16)?.try_into().unwrap(),
            counter: u32::from_le_bytes(r.take(4)?.try_into().unwrap()),
        };
        if !v.valid() {
            return Err(Error::Corrupt);
        }
        Ok(v)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Keys {
    Classic {
        #[serde(with = "crate::hex")]
        key: [u8; 16],
        kind: u8,
    },
    Ble {
        local: Security,
        peer: Security,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bond {
    pub owner: u64,
    pub identity: Peer,
    pub complete: bool,
    pub keys: Keys,
}
impl Bond {
    pub fn valid(&self) -> bool {
        self.owner != 0
            && self.complete
            && match (&self.keys, self.identity.transport) {
                (Keys::Classic { kind, .. }, Transport::Classic) => {
                    !self.identity.random && *kind <= 8
                }
                (Keys::Ble { local, peer }, Transport::Ble) => {
                    local.valid() && peer.valid() && (local.flags | peer.flags) & 1 != 0
                }
                _ => false,
            }
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        out.try_reserve_exact(145).map_err(|_| Error::Unavailable)?;
        out.extend_from_slice(&self.owner.to_le_bytes());
        codec::peer(&mut out, self.identity);
        out.push(u8::from(self.complete));
        match &self.keys {
            Keys::Classic { key, kind } => {
                out.extend_from_slice(key);
                out.push(*kind);
            }
            Keys::Ble { local, peer } => {
                local.encode(&mut out);
                peer.encode(&mut out);
            }
        }
        Ok(out)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(bytes);
        let owner = r.u64()?;
        let identity = r.peer()?;
        let complete = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(Error::Corrupt),
        };
        let keys = match identity.transport {
            Transport::Classic => Keys::Classic {
                key: r.take(16)?.try_into().unwrap(),
                kind: r.u8()?,
            },
            Transport::Ble => Keys::Ble {
                local: Security::read(&mut r)?,
                peer: Security::read(&mut r)?,
            },
        };
        r.finish()?;
        Ok(Self {
            owner,
            identity,
            complete,
            keys,
        })
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRecord {
    pub policy: crate::devices::Policy,
    pub bond: Bond,
}
pub async fn load<S: RecordStore>(store: &mut S, id: u64) -> Result<Option<Bond>, Error> {
    let Some(bytes) = store.load_owned(record_key(2, id)).await? else {
        return Ok(None);
    };
    let record: DeviceRecord = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
    Ok(Some(record.bond))
}
pub async fn commit<S: RecordStore>(
    store: &mut S,
    policy: &crate::devices::Policy,
    bond: &Bond,
) -> Result<(), Error> {
    if !policy.valid() || !bond.valid() || bond.owner != policy.id || bond.identity != policy.peer {
        return Err(Error::Corrupt);
    }
    store
        .save(
            record_key(2, policy.id),
            &crate::storage::json(&DeviceRecord {
                policy: policy.clone(),
                bond: bond.clone(),
            })?,
        )
        .await
}
/// Bluetooth ah(): test a canonical most-significant-byte-first private address
/// against a peer IRK. This is also used when native inventory requires local keys.
pub fn resolves_rpa(irk: &[u8; 16], address: &[u8; 6]) -> bool {
    use aes::cipher::{BlockEncrypt, KeyInit};
    if address[0] & 0xc0 != 0x40 {
        return false;
    }
    let mut block = aes::cipher::generic_array::GenericArray::default();
    block[13..].copy_from_slice(&address[..3]);
    aes::Aes128::new_from_slice(irk)
        .unwrap()
        .encrypt_block(&mut block);
    block[13..] == address[3..]
}
