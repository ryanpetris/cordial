//! Portable bond records and the device records that hold them. Native adapters translate their
//! database callbacks to these fields; committed records are selected only by a device's bond ID.
use crate::model::identifiers::Transport;
use crate::{
    codec::{self, Reader},
    devices::{Peer, Policy},
    storage::{self, Error, RecordStore, record_key},
};
use alloc::vec::Vec;
use cordial_protocol::storage as saved;
pub const MAINTENANCE_BYTES: usize = 32768;
pub const PAIR_BYTES: usize = 16384;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Security {
    /// LTK, IRK, CSRK present; authenticated, authorized, secure connections.
    pub flags: u8,
    pub key_size: u8,
    pub ediv: u16,
    pub rand: [u8; 8],
    pub ltk: [u8; 16],
    pub irk: [u8; 16],
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
    fn saved(&self) -> saved::Security {
        saved::Security {
            flags: self.flags.into(),
            key_size: self.key_size.into(),
            ediv: self.ediv.into(),
            rand: self.rand.into(),
            ltk: self.ltk.into(),
            irk: self.irk.into(),
            csrk: self.csrk.into(),
            counter: self.counter,
        }
    }
    fn from_saved(security: Option<saved::Security>) -> Result<Self, Error> {
        let security = security.ok_or(Error::Corrupt)?;
        Ok(Self {
            flags: storage::narrow(security.flags)?,
            key_size: storage::narrow(security.key_size)?,
            ediv: storage::narrow(security.ediv)?,
            rand: storage::array(&security.rand)?,
            ltk: storage::array(&security.ltk)?,
            irk: storage::array(&security.irk)?,
            csrk: storage::array(&security.csrk)?,
            counter: security.counter,
        })
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keys {
    Classic { key: [u8; 16], kind: u8 },
    Ble { local: Security, peer: Security },
}
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// The binary form native adapters exchange.
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
    /// The saved form of a complete bond.
    pub fn saved(&self) -> saved::Bond {
        saved::Bond {
            owner: self.owner,
            identity: Some(self.identity.saved()),
            keys: Some(match &self.keys {
                Keys::Classic { key, kind } => saved::bond::Keys::Classic(saved::ClassicKeys {
                    link_key: key.as_slice().into(),
                    key_type: (*kind).into(),
                }),
                Keys::Ble { local, peer } => saved::bond::Keys::Ble(saved::BleKeys {
                    local: Some(local.saved()),
                    peer: Some(peer.saved()),
                }),
            }),
        }
    }
    /// The bond a record holds, not yet validated. Saved bonds are complete.
    pub fn from_saved(bond: Option<saved::Bond>) -> Result<Self, Error> {
        let bond = bond.ok_or(Error::Corrupt)?;
        Ok(Self {
            owner: bond.owner,
            identity: Peer::from_saved(bond.identity)?,
            complete: true,
            keys: match bond.keys.ok_or(Error::Corrupt)? {
                saved::bond::Keys::Classic(keys) => Keys::Classic {
                    key: storage::array(&keys.link_key)?,
                    kind: storage::narrow(keys.key_type)?,
                },
                saved::bond::Keys::Ble(keys) => Keys::Ble {
                    local: Security::from_saved(keys.local)?,
                    peer: Security::from_saved(keys.peer)?,
                },
            },
        })
    }
    /// The bond in the binary form native adapters exchange.
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
/// The saved record of device `policy.id`: its policy and its bond.
pub fn record(policy: &Policy, bond: &Bond) -> Result<Vec<u8>, Error> {
    storage::encode(&saved::Device {
        policy: Some(policy.saved()),
        bond: Some(bond.saved()),
    })
}
/// The policy of device `id`'s saved record.
pub fn read_policy(id: u64, bytes: &[u8]) -> Result<Policy, Error> {
    read_record(id, bytes).map(|(policy, _)| policy)
}
/// The policy and bond of device `id`'s saved record. Only the policy is validated.
pub fn read_record(id: u64, bytes: &[u8]) -> Result<(Policy, Bond), Error> {
    let record: saved::Device = storage::decode(bytes)?;
    let mut policy = Policy::from_saved(record.policy)?;
    policy.bond = id;
    if !policy.valid() || policy.id != id {
        return Err(Error::Corrupt);
    }
    Ok((policy, Bond::from_saved(record.bond)?))
}
/// The policy and bond of a saved device record, when the record decodes and holds a complete
/// bond that belongs to it. Anything else means the record is lost.
pub fn decode(id: u64, bytes: &[u8]) -> Option<(Policy, Bond)> {
    let (policy, bond) = read_record(id, bytes).ok()?;
    belongs(&policy, &bond).then_some((policy, bond))
}
/// Whether `bond` is complete and belongs to the device `policy` describes.
pub fn belongs(policy: &Policy, bond: &Bond) -> bool {
    bond.valid() && bond.owner == policy.id && bond.identity == policy.peer
}
pub async fn load<S: RecordStore>(store: &mut S, id: u64) -> Result<Option<Bond>, Error> {
    let Some(bytes) = store.load_owned(record_key(2, id)).await? else {
        return Ok(None);
    };
    let record: saved::Device = storage::decode(&bytes)?;
    Bond::from_saved(record.bond).map(Some)
}
pub async fn commit<S: RecordStore>(
    store: &mut S,
    policy: &Policy,
    bond: &Bond,
) -> Result<(), Error> {
    if !policy.valid() || !belongs(policy, bond) {
        return Err(Error::Corrupt);
    }
    store
        .save(record_key(2, policy.id), &record(policy, bond)?)
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
